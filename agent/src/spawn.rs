//! Turning an `ExecSpec` into a running process the way `docker exec` would.

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use axum::http::StatusCode;
use serde::Deserialize;

use crate::users::{self, UserInfo};
use crate::Config;

/// The spec's ExecSpec. `user` may be omitted, meaning the agent's own user —
/// as an exec with no `User` runs as the container's configured user (root).
#[derive(Debug, Clone, Deserialize)]
pub struct ExecSpec {
    pub cmd: Vec<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub tty: bool,
}

/// Docker's default `PATH` when the container env has none
/// (`CreateDaemonEnvironment` → `oci.DefaultPathEnv`).
const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// An exec ready to start.
pub struct Prepared {
    program: PathBuf,
    argv: Vec<String>,
    env: Vec<(OsString, OsString)>,
    cwd: CString,
    user: UserInfo,
    /// Only when running as root. A non-root agent can only ever run as
    /// itself (anything else is refused with 403 before we get here), and
    /// calling `setgroups` unprivileged would fail the exec.
    switch_ids: bool,
}

/// Why a spec could not become a process.
pub enum PrepareError {
    /// An HTTP-level refusal (bad request, wrong user for a non-root agent).
    Refused(StatusCode, String),
    /// The command cannot start. Docker reports this as a *finished* exec with
    /// 126/127 and the reason on stderr, not as an API error, so callers turn
    /// it into exactly that.
    CannotStart { code: i32, message: String },
}

impl PrepareError {
    fn cannot_start(code: i32, message: String) -> Self {
        PrepareError::CannotStart { code, message }
    }
}

/// Validate a spec and resolve user, env, working directory and program.
pub fn prepare(cfg: &Config, spec: &ExecSpec, tty: bool) -> Result<Prepared, PrepareError> {
    if spec.cmd.is_empty() || spec.cmd[0].is_empty() {
        return Err(PrepareError::Refused(StatusCode::BAD_REQUEST, "cmd must be non-empty".into()));
    }
    if spec.cmd.iter().any(|a| a.contains('\0')) {
        return Err(PrepareError::Refused(StatusCode::BAD_REQUEST, "cmd contains a NUL byte".into()));
    }

    let me = users::current();
    let is_root = unsafe { libc::geteuid() } == 0;
    let user = match spec.user.as_deref().filter(|u| !u.is_empty()) {
        Some(name) => users::lookup(name).ok_or_else(|| {
            // runc's wording, which is what `docker exec -u nobody-here` prints.
            PrepareError::cannot_start(
                126,
                format!("unable to find user {name}: no matching entries in passwd file"),
            )
        })?,
        None => me.clone().ok_or_else(|| {
            PrepareError::Refused(
                StatusCode::INTERNAL_SERVER_ERROR,
                "the agent's own user has no passwd entry".into(),
            )
        })?,
    };
    if !is_root && user.uid != unsafe { libc::geteuid() } {
        return Err(PrepareError::Refused(
            StatusCode::FORBIDDEN,
            format!("the agent is not root and cannot run as {}", user.name),
        ));
    }

    let env = merge_env(cfg, spec, &user, tty);

    // `working_dir: null` is `docker exec` without `-w`: the container's
    // WorkingDir, which is the agent's cwd at startup. Checked here so a bad
    // directory is a clean 126 with runc's message rather than an errno that
    // happens to collide with "no such binary".
    let cwd = match &spec.working_dir {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => cfg.default_cwd.clone(),
    };
    if !cwd.is_dir() {
        return Err(PrepareError::cannot_start(
            126,
            format!(
                "chdir to cwd (\"{}\") set in config.json failed: no such file or directory",
                cwd.display()
            ),
        ));
    }

    let program = resolve_program(&spec.cmd[0], &env, &cwd).ok_or_else(|| {
        // runc's text for a missing binary; Docker reports it with 127 the
        // way a shell does, and the app's callers check that code.
        PrepareError::cannot_start(
            127,
            format!(
                "OCI runtime exec failed: exec failed: unable to start container process: exec: \"{}\": executable file not found in $PATH: unknown",
                spec.cmd[0]
            ),
        )
    })?;

    Ok(Prepared {
        program,
        argv: spec.cmd.clone(),
        env,
        cwd: CString::new(cwd.as_os_str().as_bytes()).map_err(|_| {
            PrepareError::Refused(StatusCode::BAD_REQUEST, "working_dir contains a NUL byte".into())
        })?,
        switch_ids: is_root,
        user,
    })
}

/// Build the exec's environment.
///
/// Order, matching Docker's `ReplaceOrAppendEnvValues(CreateDaemonEnvironment(…), spec.Env)`:
/// the base env; `PATH` defaulted if absent and `TERM=xterm` for a tty if
/// absent (both Docker daemon defaults); then the spec's entries, later ones
/// winning. An entry with no `=` *removes* that variable, as Docker does.
/// Finally `HOME`/`USER`/`LOGNAME` come from passwd unless the **spec** set
/// them — the agent's own `HOME=/root` must not leak into a `claude` exec.
fn merge_env(cfg: &Config, spec: &ExecSpec, user: &UserInfo, tty: bool) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = cfg.base_env.clone();
    fn set(env: &mut Vec<(OsString, OsString)>, k: &OsStr, v: &OsStr) {
        match env.iter_mut().find(|(ek, _)| ek == k) {
            Some(slot) => slot.1 = v.to_os_string(),
            None => env.push((k.to_os_string(), v.to_os_string())),
        }
    }
    if !env.iter().any(|(k, _)| k == "PATH") {
        set(&mut env, OsStr::new("PATH"), OsStr::new(DEFAULT_PATH));
    }
    if tty && !env.iter().any(|(k, _)| k == "TERM") {
        set(&mut env, OsStr::new("TERM"), OsStr::new("xterm"));
    }
    let mut spec_set: Vec<&str> = Vec::new();
    for entry in &spec.env {
        match entry.split_once('=') {
            Some((k, v)) => {
                set(&mut env, OsStr::new(k), OsStr::new(v));
                spec_set.push(k);
            }
            None => {
                env.retain(|(k, _)| k != entry.as_str());
                spec_set.push(entry);
            }
        }
    }
    for (k, v) in [("HOME", &user.home), ("USER", &user.name), ("LOGNAME", &user.name)] {
        if !spec_set.contains(&k) {
            set(&mut env, OsStr::new(k), OsStr::new(v));
        }
    }
    env.retain(|(k, v)| {
        !k.is_empty() && !k.as_bytes().contains(&b'=') && !k.as_bytes().contains(&0) && !v.as_bytes().contains(&0)
    });
    env
}

/// `cmd[0]` through the **merged** env's `PATH` (spec), like runc's
/// `LookPath`. A name containing `/` is used as given (relative to the
/// working directory, as it will be after `chdir`).
fn resolve_program(cmd0: &str, env: &[(OsString, OsString)], cwd: &Path) -> Option<PathBuf> {
    if cmd0.contains('/') {
        return Some(PathBuf::from(cmd0));
    }
    let path = env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.clone())?;
    for dir in std::env::split_paths(&path) {
        let dir = if dir.as_os_str().is_empty() { cwd.to_path_buf() } else { cwd.join(dir) };
        let candidate = dir.join(cmd0);
        if let Ok(meta) = std::fs::metadata(&candidate) {
            if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                return Some(candidate);
            }
        }
    }
    None
}

/// What the child's stdio should be.
pub enum Io {
    Null,
    Piped,
    Fd(std::os::fd::OwnedFd),
}

impl Io {
    fn stdio(self) -> Stdio {
        match self {
            Io::Null => Stdio::null(),
            Io::Piped => Stdio::piped(),
            Io::Fd(fd) => Stdio::from(fd),
        }
    }
}

impl Prepared {
    /// Spawn. `controlling_tty` makes stdin (which must be a pty slave) the
    /// session's controlling terminal.
    ///
    /// Every exec runs in **its own session, hence its own process group**
    /// (`setsid`), so the output-limit kill and the disconnect `SIGHUP` reach
    /// everything it started, not just the first pid.
    pub fn spawn(
        &self,
        stdin: Io,
        stdout: Io,
        stderr: Io,
        controlling_tty: bool,
    ) -> Result<tokio::process::Child, PrepareError> {
        let mut cmd = std::process::Command::new(&self.program);
        cmd.arg0(&self.argv[0])
            .args(&self.argv[1..])
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(stdin.stdio())
            .stdout(stdout.stdio())
            .stderr(stderr.stdio());

        let switch = self.switch_ids;
        let (uid, gid) = (self.user.uid, self.user.gid);
        let groups = self.user.groups.clone();
        let cwd = self.cwd.clone();
        // Runs in the forked child: only async-signal-safe syscalls, nothing
        // that allocates (everything it touches was built above).
        //
        // Order matters. `setsid` first (new session and process group). Then
        // the controlling tty, which needs a session leader. Then gid,
        // supplementary groups (what `initgroups` sets — resolved in the
        // parent, see `UserInfo::groups`), and uid last, since dropping uid
        // first would forfeit the right to change the other two. `chdir` comes
        // after the switch so a directory the user cannot enter fails the way
        // it does under runc, rather than succeeding as root.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if controlling_tty && libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                if switch {
                    if libc::setgid(gid) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::setgroups(groups.len() as _, groups.as_ptr()) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::setuid(uid) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                if libc::chdir(cwd.as_ptr()) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        // Never `kill_on_drop`: a dropped `docker exec` stream does not kill
        // the process, and neither does losing our handle to it.
        tokio::process::Command::from(cmd).spawn().map_err(|e| {
            // ENOENT here means a relative or explicit path that was not
            // there (PATH lookups were checked already): 127 like a shell.
            // Anything else — EACCES, ENOEXEC, a failed setuid — is 126,
            // "found but could not run".
            let code = if e.kind() == io::ErrorKind::NotFound { 127 } else { 126 };
            PrepareError::cannot_start(
                code,
                format!(
                    "OCI runtime exec failed: exec failed: unable to start container process: exec: \"{}\": {e}: unknown",
                    self.argv[0]
                ),
            )
        })
    }
}

/// Docker's exit code for a process: its status, or 128 + signal when a
/// signal killed it (what `docker inspect` and a shell both report).
pub fn exit_code(status: ExitStatus) -> i32 {
    match (status.code(), status.signal()) {
        (Some(c), _) => c,
        (None, Some(s)) => 128 + s,
        (None, None) => 255,
    }
}

/// Signal a whole exec: its pid is its process-group id (it called `setsid`).
pub fn signal_group(pid: Option<u32>, sig: libc::c_int) {
    if let Some(pid) = pid.filter(|&p| p > 0) {
        unsafe {
            libc::kill(-(pid as libc::pid_t), sig);
        }
    }
}
