//! Where a project's container runs, and the one interface the rest of the app
//! uses to reach inside it.
//!
//! Spec: `docs/superpowers/specs/2026-11-13-aws-ecs-host-design.md` ("Host
//! abstraction"). This is phase 1 of that plan: every operation that happens
//! *inside* a project container — one-shot and attached execs, file transfer,
//! "is it running" — goes through [`ContainerTarget`] instead of naming
//! `docker::exec` or bollard directly. Today the only variant is
//! [`ContainerTarget::Docker`], which delegates to the existing code in
//! `docker/exec.rs` and `docker/container.rs` with no change in behaviour. An
//! AWS variant (reached through the in-container agent) slots in beside it.
//!
//! ## Why an enum and not a trait object
//!
//! The set of hosts is closed and known at compile time, every method is async,
//! and a `dyn` async trait would need boxing on every call (or the
//! `async-trait` crate) for no benefit. Adding a host means adding a variant and
//! letting the compiler point at every `match` that needs an arm.
//!
//! ## What does *not* go through here
//!
//! Things that only make sense for the local Docker host: image build/pull,
//! base-image migration, snapshot commits, volumes, the LiteLLM gateway and STT
//! sidecars, and (until phase 1b) container create/start/stop and the `die`
//! event stream. Those keep calling `crate::docker` directly. The
//! `no_container_interior_bollard_outside_the_host_layer` test pins which files
//! may still do so.

pub mod sessions;

pub use crate::docker::exec::{
    MAX_DROP_BYTES, MAX_ONESHOT_OUTPUT, OUTPUT_LIMIT_MARKER, PROC_NET_OUTPUT_LIMIT,
};
pub use sessions::ExecSessionManager;

use bytes::Bytes;
use futures_util::Stream;
use std::pin::Pin;
use tokio::io::AsyncWrite;

use crate::docker;
use crate::models::Project;

/// One chunk of output from an exec, already demultiplexed.
///
/// Host-neutral stand-in for bollard's `LogOutput`. With `tty = true` there is
/// no stdout/stderr split and everything arrives as [`ExecChunk::StdOut`] (Docker
/// reports a TTY stream as `Console`, which maps here too — consumers that cared
/// treated the two the same).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecChunk {
    StdOut(Bytes),
    StdErr(Bytes),
    /// stdin echo or anything else a host might surface. Consumers ignore it.
    Other(Bytes),
}

impl ExecChunk {
    pub fn into_bytes(self) -> Bytes {
        match self {
            ExecChunk::StdOut(b) | ExecChunk::StdErr(b) | ExecChunk::Other(b) => b,
        }
    }
}

pub type ExecOutputStream = Pin<Box<dyn Stream<Item = Result<ExecChunk, String>> + Send>>;

/// An exec that has been created and started with its streams attached.
///
/// `input` is a sink that is never read when the exec was opened with
/// `attach_stdin: false`.
pub struct AttachedExec {
    pub exec_id: String,
    pub output: ExecOutputStream,
    pub input: Pin<Box<dyn AsyncWrite + Send>>,
}

/// Everything about an attached exec except where it runs.
///
/// Defaults match what this app has always used: run as `claude` in
/// `/workspace`, no TTY, stdin attached, no extra env.
#[derive(Debug, Clone)]
pub struct ExecSpec {
    pub cmd: Vec<String>,
    pub user: String,
    pub working_dir: Option<String>,
    pub env: Vec<String>,
    pub tty: bool,
    pub attach_stdin: bool,
}

impl ExecSpec {
    pub fn new(cmd: Vec<String>) -> Self {
        Self {
            cmd,
            user: "claude".to_string(),
            working_dir: Some("/workspace".to_string()),
            env: Vec::new(),
            tty: false,
            attach_stdin: true,
        }
    }
    pub fn user(mut self, user: &str) -> Self {
        self.user = user.to_string();
        self
    }
    pub fn working_dir(mut self, dir: Option<&str>) -> Self {
        self.working_dir = dir.map(str::to_string);
        self
    }
    pub fn env(mut self, env: Vec<String>) -> Self {
        self.env = env;
        self
    }
    pub fn tty(mut self, tty: bool) -> Self {
        self.tty = tty;
        self
    }
    pub fn no_stdin(mut self) -> Self {
        self.attach_stdin = false;
        self
    }
}

/// A byte stream of a tar archive read out of a container.
pub type ArchiveStream = Pin<Box<dyn Stream<Item = Result<Bytes, String>> + Send>>;

/// A specific project container on a specific host.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContainerTarget {
    /// A container on the local Docker daemon, by id or name.
    Docker { container_id: String },
}

impl ContainerTarget {
    pub fn docker(container_id: impl Into<String>) -> Self {
        ContainerTarget::Docker {
            container_id: container_id.into(),
        }
    }

    /// The target for a project's container, if it has one yet.
    pub fn for_project(project: &Project) -> Option<Self> {
        project.container_id.as_deref().map(Self::docker)
    }

    /// The host-specific identifier (a Docker container id today). For logs
    /// and for keying per-container state; never parse it.
    pub fn id(&self) -> &str {
        match self {
            ContainerTarget::Docker { container_id } => container_id,
        }
    }

    // ── state ────────────────────────────────────────────────────────────────

    /// Whether the container is running. An inspect failure (gone, daemon
    /// down) reads as `false`, as `docker::is_container_running` always has.
    pub async fn is_running(&self) -> Result<bool, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::container::is_container_running(container_id).await
            }
        }
    }

    // ── one-shot execs ───────────────────────────────────────────────────────

    /// Run as `claude`, return combined output. Errors on a non-UTF-8-safe
    /// transport failure only; a non-zero exit is *not* an error here.
    pub async fn exec_oneshot(&self, cmd: Vec<String>) -> Result<String, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::exec_oneshot(container_id, cmd).await
            }
        }
    }

    pub async fn exec_oneshot_env(
        &self,
        cmd: Vec<String>,
        env: Vec<String>,
    ) -> Result<String, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::exec_oneshot_env(container_id, cmd, env).await
            }
        }
    }

    pub async fn exec_oneshot_env_status(
        &self,
        cmd: Vec<String>,
        env: Vec<String>,
    ) -> Result<(String, i64), String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::exec_oneshot_env_status(container_id, cmd, env).await
            }
        }
    }

    pub async fn exec_oneshot_as(
        &self,
        user: &str,
        cmd: Vec<String>,
        env: Vec<String>,
    ) -> Result<(String, i64), String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::exec_oneshot_as(container_id, user, cmd, env).await
            }
        }
    }

    pub async fn exec_oneshot_limited(
        &self,
        user: &str,
        cmd: Vec<String>,
        limit: usize,
    ) -> Result<(String, i64), String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::exec_oneshot_limited(container_id, user, cmd, limit).await
            }
        }
    }

    /// stdout and stderr kept apart: `(stdout, stderr, exit_code)`.
    pub async fn exec_oneshot_streams_as(
        &self,
        user: &str,
        cmd: Vec<String>,
        env: Vec<String>,
    ) -> Result<(String, String, i64), String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::exec_oneshot_streams_as(container_id, user, cmd, env).await
            }
        }
    }

    /// Start a command and return without waiting or reading its output.
    /// Returns the exec id.
    pub async fn exec_detached(&self, spec: ExecSpec) -> Result<String, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::exec_detached(container_id, &spec).await
            }
        }
    }

    // ── attached execs ───────────────────────────────────────────────────────

    /// Run as `claude` in `/workspace` with stdin attached.
    pub async fn attach_exec(&self, cmd: Vec<String>, tty: bool) -> Result<AttachedExec, String> {
        self.attach(ExecSpec::new(cmd).tty(tty)).await
    }

    pub async fn attach(&self, spec: ExecSpec) -> Result<AttachedExec, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::create_attached_exec_spec(container_id, &spec).await
            }
        }
    }

    /// The exit code of an exec this target started, once it has finished.
    /// `None` if the host could not say.
    pub async fn wait_exec_exit(&self, exec_id: &str) -> Option<i64> {
        match self {
            ContainerTarget::Docker { .. } => docker::exec::wait_for_exec_exit(exec_id).await,
        }
    }

    pub async fn resize_exec(&self, exec_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        match self {
            ContainerTarget::Docker { .. } => docker::exec::resize_exec(exec_id, cols, rows).await,
        }
    }

    // ── files ────────────────────────────────────────────────────────────────

    /// The container user's `(uid, gid)`, falling back to `(1000, 1000)`.
    pub async fn user_ids(&self) -> (u64, u64) {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::container_user_ids(container_id).await
            }
        }
    }

    /// Write `data` to `dest_dir/file_name` as root-owned with `mode`.
    pub async fn upload_bytes(
        &self,
        dest_dir: &str,
        file_name: &str,
        data: &[u8],
        mode: u32,
    ) -> Result<String, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::upload_bytes_to_container(
                    container_id,
                    dest_dir,
                    file_name,
                    data,
                    mode,
                )
                .await
            }
        }
    }

    /// [`Self::upload_bytes`] with the owner set to `ids`.
    pub async fn upload_bytes_with_ids(
        &self,
        dest_dir: &str,
        file_name: &str,
        data: &[u8],
        mode: u32,
        ids: (u64, u64),
    ) -> Result<String, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::upload_bytes_with_ids(
                    container_id,
                    dest_dir,
                    file_name,
                    data,
                    mode,
                    ids,
                )
                .await
            }
        }
    }

    /// Copy a host file in, owned by the container user.
    pub async fn upload_host_file(
        &self,
        host_path: &str,
        dest_dir: &str,
        dest_name: &str,
    ) -> Result<String, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::upload_host_file_to_container(
                    container_id,
                    host_path,
                    dest_dir,
                    dest_name,
                )
                .await
            }
        }
    }

    /// [`Self::upload_host_file`] with the owner ids already known.
    pub async fn upload_host_file_with_ids(
        &self,
        host_path: &str,
        dest_dir: &str,
        dest_name: &str,
        ids: (u64, u64),
    ) -> Result<String, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::upload_host_file_with_ids(
                    container_id,
                    host_path,
                    dest_dir,
                    dest_name,
                    ids,
                )
                .await
            }
        }
    }

    /// Read `path` out of the container as a tar stream (Docker's archive
    /// API). Dropping the stream cancels the transfer.
    pub fn download_archive(&self, path: &str) -> ArchiveStream {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::download_archive(container_id, path)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_spec_defaults_match_what_the_app_always_used() {
        let s = ExecSpec::new(vec!["true".into()]);
        assert_eq!(s.user, "claude");
        assert_eq!(s.working_dir.as_deref(), Some("/workspace"));
        assert!(s.env.is_empty());
        assert!(!s.tty);
        assert!(s.attach_stdin);
    }

    #[test]
    fn for_project_follows_the_container_id() {
        let mut p: Project = serde_json::from_value(serde_json::json!({
            "id": "p1", "name": "p", "paths": [], "container_id": null,
            "status": "stopped", "backend": "anthropic",
            "allow_docker_access": false, "full_permissions": false,
            "created_at": "now", "updated_at": "now"
        }))
        .expect("minimal project deserializes");
        assert_eq!(ContainerTarget::for_project(&p), None);
        p.container_id = Some("abc".into());
        assert_eq!(
            ContainerTarget::for_project(&p),
            Some(ContainerTarget::docker("abc"))
        );
        assert_eq!(ContainerTarget::docker("abc").id(), "abc");
    }

    /// Phase 1's boundary, pinned: outside `docker/` and `host/`, only the
    /// files listed here may name bollard or `docker::exec`. Each entry is
    /// Docker-only by nature (migration, snapshots, sidecars) or is scheduled
    /// for phase 1b (lifecycle, the `die` event stream). Shrink this list; do
    /// not grow it — new container-interior code goes through
    /// [`ContainerTarget`].
    #[test]
    fn no_container_interior_bollard_outside_the_host_layer() {
        const ALLOWED: &[&str] = &[
            // Docker-only: base-image migration replays apt / restores a payload.
            "commands/migration_commands.rs",
            // Phase 1b: lifecycle and the `die` event stream.
            "container_watch.rs",
            // Phase 1a, not yet migrated. Each migration removes its own
            // entries; the list must be empty before phase 1a merges.
            "auth_bridge/mod.rs",
            "auth_bridge/tunnel.rs",
            "browser_view/detect.rs",
            "browser_view/install.rs",
            "browser_view/mod.rs",
            "browser_view/page.rs",
            "commands/auth_token_commands.rs",
            "commands/file_commands.rs",
            "commands/inspect_commands.rs",
            "commands/terminal_commands.rs",
            "file_viewer/poll.rs",
            "file_viewer/resolve.rs",
            "marketplace/gh_login.rs",
            "marketplace/sync.rs",
        ];
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let rel = path
                    .strip_prefix(&src)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if path.is_dir() {
                    if rel != "docker" && rel != "host" {
                        stack.push(path);
                    }
                    continue;
                }
                if !rel.ends_with(".rs") || ALLOWED.contains(&rel.as_str()) {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                if text.contains("bollard::")
                    || text.contains("docker::exec")
                    || text.contains("get_docker")
                {
                    offenders.push(rel);
                }
            }
        }
        offenders.sort();
        assert!(
            offenders.is_empty(),
            "these files reach into the container without going through host::ContainerTarget: {:?}",
            offenders
        );
    }
}
