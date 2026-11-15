//! The container side of an AWS workspace (`TRIPLEC_HOST=aws`): the marked
//! blocks of `entrypoint.sh` and the two scripts it drives, run under bash in
//! temp dirs — the `sync_host_ssh` pattern in `image.rs`. Root-only steps
//! (chown, `su claude`) are switched off by text substitution, so these run
//! the same as root (CI) and as a user.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

const ENTRYPOINT: &str = include_str!("../../../../container/entrypoint.sh");
const WIP_PUSH: &str = include_str!("../../../../container/triple-c-wip-push");
const IDLE_WATCH: &str = include_str!("../../../../container/triple-c-idle-watch");
const SCHEDULER: &str = include_str!("../../../../container/triple-c-scheduler");

fn block<'a>(text: &'a str, name: &str) -> &'a str {
    let start = text
        .find(&format!("# >>> {name}\n"))
        .unwrap_or_else(|| panic!("start marker {name}"));
    let end = text
        .find(&format!("# <<< {name}\n"))
        .unwrap_or_else(|| panic!("end marker {name}"));
    &text[start..end]
}

/// The aws blocks with the container user switched off: no `su claude`, no
/// chown to a user the test host may not have.
fn aws_blocks(names: &[&str]) -> String {
    names
        .iter()
        .map(|n| block(ENTRYPOINT, n).replace("triplec_run_as=claude", "triplec_run_as="))
        .collect::<Vec<_>>()
        .join("\n")
}

fn bash(script: &str, args: &[&Path], env: &[(&str, &str)], clear_env: &[&str]) -> Output {
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(script).arg("test");
    cmd.args(args);
    for k in clear_env {
        cmd.env_remove(k);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run bash")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn mode(p: &Path) -> u32 {
    fs::metadata(p).unwrap().permissions().mode() & 0o777
}

fn b64(s: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(s)
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// ------------------------------------------------------------ secret files

fn files_script(tmp: &Path) -> String {
    let ssh = tmp.join("host-ssh");
    let ca = tmp.join("host-ca");
    let shm = tmp.join("shm");
    let blocks = aws_blocks(&["aws_env"])
        .replace("/tmp/.host-ssh", ssh.to_str().unwrap())
        .replace("/tmp/.host-ca", ca.to_str().unwrap());
    // The agent-token block too, so the snapshot it takes is checked; its
    // token file goes to the temp dir rather than the real /dev/shm.
    let agent = block(ENTRYPOINT, "agent_token").replace("/dev/shm/triple-c", shm.to_str().unwrap());
    format!(
        "{blocks}\n{agent}\n\
         env -0 > \"$1/env\"\n\
         printf '%s\\0' \"${{agent_env[@]}}\" > \"$1/agent_env\"\n\
         printf '%s' \"${{triplec_repos_json:-}}\" > \"$1/repos\"\n"
    )
}

#[test]
fn aws_files_decode_to_the_host_dirs_mode_600_and_leave_the_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let files = format!(
        r#"{{"ssh/id_ed25519":"{}","ssh/config":"{}","ca/corp.crt":"{}","ssh/..":"{}","ssh/a/b":"{}","ssh/":"{}","home/x":"{}","ca/bad":"%%%"}}"#,
        b64("PRIVATE-KEY-BODY"),
        b64("Host *\n"),
        b64("-----BEGIN CERTIFICATE-----"),
        b64("x"),
        b64("x"),
        b64("x"),
        b64("x"),
    );
    let repos = r#"[{"url":"https://example.invalid/r.git","branch":null,"folder":"r"}]"#;
    let out = bash(
        &files_script(tmp.path()),
        &[tmp.path()],
        &[
            ("TRIPLEC_HOST", "aws"),
            ("TRIPLEC_FILES", &files),
            ("TRIPLEC_REPOS", repos),
            ("TRIPLEC_AGENT_TOKEN", "test-token-not-real"),
        ],
        &[],
    );
    let log = text(&out);
    assert!(out.status.success(), "{log}");

    let ssh = tmp.path().join("host-ssh");
    let ca = tmp.path().join("host-ca");
    assert_eq!(fs::read_to_string(ssh.join("id_ed25519")).unwrap(), "PRIVATE-KEY-BODY");
    assert_eq!(fs::read_to_string(ssh.join("config")).unwrap(), "Host *\n");
    assert_eq!(fs::read_to_string(ca.join("corp.crt")).unwrap(), "-----BEGIN CERTIFICATE-----");
    for f in [ssh.join("id_ed25519"), ssh.join("config"), ca.join("corp.crt")] {
        assert_eq!(mode(&f), 0o600, "{}", f.display());
    }
    assert_eq!(mode(&ssh), 0o700);
    assert_eq!(mode(&ca), 0o700);

    // Exactly the three good files; every bad name refused out loud.
    let mut names: Vec<_> = fs::read_dir(&ssh).unwrap().map(|e| e.unwrap().file_name()).collect();
    names.sort();
    assert_eq!(names, ["config", "id_ed25519"]);
    assert_eq!(fs::read_dir(&ca).unwrap().count(), 1);
    assert!(!tmp.path().join("a").exists() && !tmp.path().join("x").exists());
    for bad in ["ssh/..", "ssh/a/b", "\"ssh/\"", "home/x", "ca/bad"] {
        assert!(log.contains(&format!("refusing")) && log.contains(bad.trim_matches('"')), "{bad}: {log}");
    }

    // Gone from the environment, and from the agent's snapshot of it.
    let env = fs::read_to_string(tmp.path().join("env")).unwrap();
    assert!(!env.contains("TRIPLEC_FILES="), "{env}");
    assert!(!env.contains("TRIPLEC_REPOS="));
    assert!(!env.contains("PRIVATE-KEY-BODY"));
    let agent_env = fs::read_to_string(tmp.path().join("agent_env")).unwrap();
    assert!(!agent_env.contains("TRIPLEC_FILES"), "{agent_env}");
    assert!(!agent_env.contains("TRIPLEC_REPOS"));
    assert!(!agent_env.contains(&b64("PRIVATE-KEY-BODY")));
    // The repo list survives as an unexported shell variable for clone_repos.
    assert_eq!(fs::read_to_string(tmp.path().join("repos")).unwrap(), repos);
}

#[test]
fn outside_aws_mode_the_files_variable_is_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let files = format!(r#"{{"ssh/id":"{}"}}"#, b64("k"));
    let out = bash(
        &files_script(tmp.path()),
        &[tmp.path()],
        &[("TRIPLEC_FILES", &files)],
        &["TRIPLEC_HOST", "TRIPLEC_AGENT_TOKEN"],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(!tmp.path().join("host-ssh").exists());
    let env = fs::read_to_string(tmp.path().join("env")).unwrap();
    assert!(env.contains("TRIPLEC_FILES="));
}

#[test]
fn malformed_files_json_writes_nothing_and_does_not_fail_the_start() {
    let tmp = tempfile::tempdir().unwrap();
    let out = bash(
        &files_script(tmp.path()),
        &[tmp.path()],
        &[("TRIPLEC_HOST", "aws"), ("TRIPLEC_FILES", "not json")],
        &["TRIPLEC_AGENT_TOKEN"],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("TRIPLEC_FILES"), "{}", text(&out));
    assert!(!tmp.path().join("host-ssh").join("x").exists());
    let env = fs::read_to_string(tmp.path().join("env")).unwrap();
    assert!(!env.contains("TRIPLEC_FILES="));
}

// -------------------------------------------------------- Hybrid home links

fn link_home(home: &Path, persist: &Path) -> Output {
    let script = format!("{}\nlink_persistent_home \"$1\" \"$2\" \"\"\n", aws_blocks(&["aws_env", "aws_home"]));
    bash(&script, &[home, persist], &[], &[])
}

#[test]
fn link_persistent_home_seeds_on_first_run_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let (home, persist) = (tmp.path().join("home"), tmp.path().join("mnt"));
    fs::create_dir_all(home.join(".claude/bin")).unwrap();
    fs::write(home.join(".claude/bin/seed"), "from-image").unwrap();
    fs::create_dir_all(home.join(".ssh")).unwrap();
    fs::write(home.join(".ssh/known_hosts"), "image-hosts").unwrap();
    fs::create_dir_all(&persist).unwrap();

    let out = link_home(&home, &persist);
    assert!(out.status.success(), "{}", text(&out));
    for name in [".claude", ".claude.json", ".ssh"] {
        let link = home.join(name);
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "{name}");
        assert_eq!(fs::read_link(&link).unwrap(), persist.join(name));
    }
    assert_eq!(fs::read_to_string(persist.join(".claude/bin/seed")).unwrap(), "from-image");
    assert_eq!(fs::read_to_string(persist.join(".ssh/known_hosts")).unwrap(), "image-hosts");
    assert_eq!(fs::read_to_string(persist.join(".claude.json")).unwrap().trim(), "{}");

    // State written through the links, then a second run on the same home.
    fs::write(home.join(".claude/session"), "kept").unwrap();
    fs::write(home.join(".claude.json"), "{\"login\":1}").unwrap();
    let out = link_home(&home, &persist);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_to_string(persist.join(".claude/session")).unwrap(), "kept");
    assert_eq!(fs::read_to_string(persist.join(".claude.json")).unwrap(), "{\"login\":1}");
    assert_eq!(fs::read_link(home.join(".claude")).unwrap(), persist.join(".claude"));

    // The next task starts from the image again: its fresh copies are
    // replaced by the links and the persisted state wins.
    fs::remove_file(home.join(".claude")).unwrap();
    fs::remove_file(home.join(".claude.json")).unwrap();
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(home.join(".claude/session"), "image-copy").unwrap();
    let out = link_home(&home, &persist);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_link(home.join(".claude")).unwrap(), persist.join(".claude"));
    assert_eq!(fs::read_to_string(home.join(".claude/session")).unwrap(), "kept");
    assert_eq!(fs::read_to_string(home.join(".claude.json")).unwrap(), "{\"login\":1}");
}

#[test]
fn link_persistent_home_without_the_mount_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    let out = link_home(&home, &tmp.path().join("absent"));
    assert!(out.status.success());
    assert!(text(&out).contains("not mounted"), "{}", text(&out));
    assert!(fs::symlink_metadata(home.join(".claude")).unwrap().is_dir());
}

#[test]
fn write_claude_json_keeps_a_linked_claude_json_linked() {
    let tmp = tempfile::tempdir().unwrap();
    let (home, persist) = (tmp.path().join("home"), tmp.path().join("mnt"));
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&persist).unwrap();
    fs::write(persist.join(".claude.json"), "{}").unwrap();
    std::os::unix::fs::symlink(persist.join(".claude.json"), home.join(".claude.json")).unwrap();
    let f = block(ENTRYPOINT, "write_claude_json");
    // chown to a user the host may not have is noise here, not the point.
    let script = format!("chown() {{ :; }}\n{f}\nCLAUDE_JSON=\"$1/.claude.json\"\nwrite_claude_json '{{\"a\":1}}'\n");
    let out = bash(&script, &[&home], &[], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(fs::symlink_metadata(home.join(".claude.json")).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_to_string(persist.join(".claude.json")).unwrap().trim(), "{\"a\":1}");
}

// --------------------------------------------------------------- clone_repos

fn bare_remote(tmp: &Path, name: &str) -> std::path::PathBuf {
    let work = tmp.join(format!("{name}-src"));
    let bare = tmp.join(format!("{name}.git"));
    fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    fs::write(work.join("README"), "hello").unwrap();
    git(&work, &["add", "README"]);
    git(&work, &["commit", "-q", "-m", "init"]);
    git(&work, &["branch", "dev"]);
    git(tmp, &["clone", "-q", "--bare", work.to_str().unwrap(), bare.to_str().unwrap()]);
    bare
}

#[test]
fn clone_repos_clones_skips_existing_and_reports_failures_without_failing() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    fs::create_dir_all(&ws).unwrap();
    let remote = bare_remote(tmp.path(), "proj");
    // An existing repo whose recorded URL is bad: skipping means no attempt.
    let existing = ws.join("existing");
    fs::create_dir_all(&existing).unwrap();
    git(&existing, &["init", "-q"]);
    let json = format!(
        r#"[{{"url":"{r}","branch":null,"folder":"main-copy"}},
            {{"url":"{r}","branch":"dev","folder":"dev-copy"}},
            {{"url":"/nonexistent/nope.git","branch":null,"folder":"existing"}},
            {{"url":"/nonexistent/nope.git","branch":null,"folder":"broken"}},
            {{"url":"{r}","branch":null,"folder":"../escape"}}]"#,
        r = remote.display()
    );
    let script = format!("{}\nclone_repos \"$JSON\" \"$1\"\necho \"rc=$?\"\n", aws_blocks(&["aws_env", "aws_repos"]));
    let out = bash(&script, &[&ws], &[("JSON", &json)], &[]);
    let log = text(&out);
    assert!(out.status.success(), "{log}");
    assert!(log.contains("rc=0"), "{log}");

    assert_eq!(fs::read_to_string(ws.join("main-copy/README")).unwrap(), "hello");
    assert_eq!(git(&ws.join("dev-copy"), &["rev-parse", "--abbrev-ref", "HEAD"]), "dev");
    assert!(log.contains("Triple-C: clone of broken failed:"), "{log}");
    assert!(!log.contains("clone of existing failed"), "{log}");
    assert!(log.contains("../escape"), "a bad folder is refused by name: {log}");
    assert!(!tmp.path().join("escape").exists());
}

#[test]
fn clone_repos_with_malformed_json_reports_and_returns_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let script = format!("{}\nclone_repos 'nope' \"$1\"; echo \"rc=$?\"\n", aws_blocks(&["aws_env", "aws_repos"]));
    let out = bash(&script, &[tmp.path()], &[], &[]);
    assert!(text(&out).contains("rc=0"), "{}", text(&out));
    assert!(text(&out).contains("TRIPLEC_REPOS"), "{}", text(&out));
}

// ------------------------------------------------------------ WIP push

fn write_script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    fs::write(&p, body).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn wip_push(ws: &Path, args: &[&str]) -> Output {
    let tmp = ws.parent().unwrap();
    let script = write_script(tmp, "triple-c-wip-push", WIP_PUSH);
    Command::new(script)
        .args(args)
        .env("TRIPLEC_WORKSPACE_ROOT", ws)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", tmp)
        .output()
        .unwrap()
}

/// A workspace holding one clone of a bare remote, with an identity set.
fn workspace_with_clone(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let remote = bare_remote(tmp, "origin");
    let ws = tmp.join("workspace");
    fs::create_dir_all(&ws).unwrap();
    git(&ws, &["clone", "-q", remote.to_str().unwrap(), "proj"]);
    let repo = ws.join("proj");
    git(&repo, &["config", "user.name", "Dev"]);
    git(&repo, &["config", "user.email", "dev@example.invalid"]);
    (ws, repo, remote)
}

fn wip_refs(remote: &Path) -> Vec<String> {
    let out = git(remote, &["for-each-ref", "--format=%(refname)", "refs/heads/triple-c/wip/"]);
    out.lines().map(str::to_string).collect()
}

#[test]
fn wip_push_pushes_the_whole_tree_and_leaves_the_repo_as_it_was() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, repo, remote) = workspace_with_clone(tmp.path());
    // An unpushed commit, a staged change and an untracked file.
    fs::write(repo.join("committed.txt"), "c").unwrap();
    git(&repo, &["add", "committed.txt"]);
    git(&repo, &["commit", "-q", "-m", "local"]);
    fs::write(repo.join("README"), "staged edit").unwrap();
    git(&repo, &["add", "README"]);
    fs::write(repo.join("untracked.txt"), "u").unwrap();
    fs::write(repo.join(".gitignore"), "ignored.log\n").unwrap();
    fs::write(repo.join("ignored.log"), "noise").unwrap();

    let status_before = git(&repo, &["status", "--porcelain"]);
    let head_before = git(&repo, &["rev-parse", "HEAD"]);
    let index_before = fs::read(repo.join(".git/index")).unwrap();

    let out = wip_push(&ws, &["--reason", "stop"]);
    assert!(out.status.success(), "{}", text(&out));

    let refs = wip_refs(&remote);
    assert_eq!(refs.len(), 1, "{refs:?}");
    let name = refs[0].trim_start_matches("refs/heads/triple-c/wip/");
    assert!(
        name.len() == 15 && name.as_bytes()[8] == b'-' && name.chars().filter(|c| c.is_ascii_digit()).count() == 14,
        "{name}"
    );
    let r = &refs[0];
    assert_eq!(git(&remote, &["show", &format!("{r}:committed.txt")]), "c");
    assert_eq!(git(&remote, &["show", &format!("{r}:README")]), "staged edit");
    assert_eq!(git(&remote, &["show", &format!("{r}:untracked.txt")]), "u");
    assert!(
        !git(&remote, &["ls-tree", "--name-only", r]).lines().any(|l| l == "ignored.log"),
        "ignored files stay out"
    );
    // The WIP commit sits on top of the user's HEAD.
    assert_eq!(git(&remote, &["rev-parse", &format!("{r}^")]), head_before);

    assert_eq!(git(&repo, &["status", "--porcelain"]), status_before);
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(fs::read(repo.join(".git/index")).unwrap(), index_before, "index untouched");
    assert_eq!(git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "main");
}

#[test]
fn wip_push_of_only_unpushed_commits_pushes_head() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, repo, remote) = workspace_with_clone(tmp.path());
    fs::write(repo.join("x"), "x").unwrap();
    git(&repo, &["add", "x"]);
    git(&repo, &["commit", "-q", "-m", "x"]);
    let out = wip_push(&ws, &[]);
    assert!(out.status.success(), "{}", text(&out));
    let refs = wip_refs(&remote);
    assert_eq!(refs.len(), 1);
    assert_eq!(git(&remote, &["rev-parse", &refs[0]]), git(&repo, &["rev-parse", "HEAD"]));
}

#[test]
fn wip_push_dry_run_prints_one_json_line_per_repo_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, repo, remote) = workspace_with_clone(tmp.path());
    git(&ws, &["clone", "-q", remote.to_str().unwrap(), "clean"]);
    fs::create_dir_all(ws.join("not-a-repo")).unwrap();
    fs::write(repo.join("x"), "x").unwrap();
    git(&repo, &["add", "x"]);
    git(&repo, &["commit", "-q", "-m", "x"]);
    fs::write(repo.join("y"), "y").unwrap();

    let out = wip_push(&ws, &["--dry-run"]);
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    let by = |f: &str| lines.iter().find(|v| v["folder"] == f).unwrap().clone();
    assert_eq!(
        by("proj"),
        serde_json::json!({"folder": "proj", "dirty": true, "unpushed": 1, "branch": "main"})
    );
    assert_eq!(
        by("clean"),
        serde_json::json!({"folder": "clean", "dirty": false, "unpushed": 0, "branch": "main"})
    );
    assert!(wip_refs(&remote).is_empty());
}

#[test]
fn wip_push_of_clean_repos_pushes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _repo, remote) = workspace_with_clone(tmp.path());
    let out = wip_push(&ws, &["--reason", "idle"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(wip_refs(&remote).is_empty());
}

#[test]
fn wip_push_fails_when_a_push_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, repo, _remote) = workspace_with_clone(tmp.path());
    git(&repo, &["remote", "set-url", "origin", "/nonexistent/gone.git"]);
    fs::write(repo.join("y"), "y").unwrap();
    let out = wip_push(&ws, &[]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("proj"), "{}", text(&out));
}

#[test]
fn wip_push_rejects_an_unknown_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    fs::create_dir_all(&ws).unwrap();
    let out = wip_push(&ws, &["--reason", "whenever"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
}

// ------------------------------------------------------------ idle watch

fn is_idle(args: &[&str]) -> bool {
    let script = format!("{}\nis_idle \"$@\"\n", block(IDLE_WATCH, "is_idle"));
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(script).arg("t").args(args);
    let out = cmd.output().unwrap();
    match out.status.code() {
        Some(0) => true,
        Some(1) => false,
        _ => panic!("is_idle {args:?}: {}", text(&out)),
    }
}

#[test]
fn is_idle_needs_every_condition() {
    // now last execs tunnels claude_running jobs_running minutes
    assert!(is_idle(&["10000", "9400", "0", "0", "0", "0", "10"]), "exactly N minutes");
    assert!(is_idle(&["10000", "1000", "0", "0", "0", "0", "10"]));
    assert!(!is_idle(&["10000", "9401", "0", "0", "0", "0", "10"]), "one second short");
    assert!(!is_idle(&["10000", "1000", "1", "0", "0", "0", "10"]), "an exec is open");
    assert!(!is_idle(&["10000", "1000", "0", "2", "0", "0", "10"]), "a tunnel is open");
    assert!(!is_idle(&["10000", "1000", "0", "0", "1", "0", "10"]), "claude is running");
    assert!(!is_idle(&["10000", "1000", "0", "0", "0", "1", "10"]), "a scheduler job runs");
    // Anything unreadable is "not idle".
    assert!(!is_idle(&["10000", "", "0", "0", "0", "0", "10"]));
    assert!(!is_idle(&["10000", "x", "0", "0", "0", "0", "10"]));
    assert!(!is_idle(&["10000", "1000", "", "0", "0", "0", "10"]));
    assert!(!is_idle(&["10000", "1000", "0", "0", "0", "0", "0"]), "0 minutes is off");
    assert!(!is_idle(&["10000", "1000", "0", "0", "0", "0", ""]));
}

struct Watch {
    tmp: tempfile::TempDir,
}

impl Watch {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        write_script(tmp.path(), "triple-c-idle-watch", IDLE_WATCH);
        write_script(tmp.path(), "triple-c-scheduler", SCHEDULER);
        fs::write(tmp.path().join("token"), "test-token-not-real").unwrap();
        fs::create_dir_all(tmp.path().join("home")).unwrap();
        Watch { tmp }
    }

    fn cmd(&self, agent_url: &str, signal_pid: u32) -> Command {
        let p = self.tmp.path();
        let mut c = Command::new(p.join("triple-c-idle-watch"));
        c.env_clear()
            .env("PATH", std::env::var("PATH").unwrap())
            .env("HOME", p.join("home"))
            .env("TRIPLEC_IDLE_STOP_MINUTES", "5")
            .env("TRIPLEC_AGENT_URL", agent_url)
            .env("TRIPLEC_AGENT_TOKEN_FILE", p.join("token"))
            .env("TRIPLEC_IDLE_INTERVAL", "1")
            .env("TRIPLEC_IDLE_SIGNAL_PID", signal_pid.to_string())
            .env("TRIPLEC_STOP_REASON_FILE", p.join("reason"))
            .env("TRIPLEC_SCHEDULER_BIN", p.join("triple-c-scheduler"))
            .env("TRIPLEC_SCHEDULER_USER", "")
            // The test host may well be running a `claude` of its own.
            .env("TRIPLEC_IDLE_PROC_NAME", "triple-c-no-such-process");
        c
    }
}

/// A one-shot fake agent: answers `/v1/activity` with `body` when the bearer
/// token matches, 401 otherwise. Returns its URL and child.
fn fake_agent(dir: &Path, body: &str) -> (String, std::process::Child) {
    let server = r#"
import http.server, sys, socketserver
body = sys.argv[1].encode()
token = open(sys.argv[2]).read()
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        ok = self.path == "/v1/activity" and self.headers.get("Authorization") == "Bearer " + token
        self.send_response(200 if ok else 401)
        self.end_headers()
        if ok: self.wfile.write(body)
    def log_message(self, *a): pass
s = socketserver.TCPServer(("127.0.0.1", 0), H)
print(s.server_address[1], flush=True)
s.serve_forever()
"#;
    let mut child = Command::new("python3")
        .arg("-I")
        .arg("-c")
        .arg(server)
        .arg(body)
        .arg(dir.join("token"))
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    use std::io::BufRead;
    std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    (format!("http://127.0.0.1:{}", line.trim()), child)
}

#[test]
fn idle_watch_check_treats_an_unreachable_agent_as_busy() {
    let w = Watch::new();
    let out = w.cmd("http://127.0.0.1:9", 0).arg("--check").output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
}

#[test]
fn idle_watch_check_reads_activity_with_the_token() {
    let w = Watch::new();
    let (url, mut agent) = fake_agent(w.tmp.path(), r#"{"last_activity_unix":1,"active_execs":0,"active_tunnels":0}"#);
    let idle = w.cmd(&url, 0).arg("--check").output().unwrap();
    let (busy_url, mut busy_agent) =
        fake_agent(w.tmp.path(), r#"{"last_activity_unix":1,"active_execs":1,"active_tunnels":0}"#);
    let busy = w.cmd(&busy_url, 0).arg("--check").output().unwrap();
    fs::write(w.tmp.path().join("token"), "wrong").unwrap();
    let unauth = w.cmd(&url, 0).arg("--check").output().unwrap();
    agent.kill().ok();
    busy_agent.kill().ok();
    assert_eq!(idle.status.code(), Some(0), "{}", text(&idle));
    assert_eq!(busy.status.code(), Some(1), "{}", text(&busy));
    assert_eq!(unauth.status.code(), Some(1), "a 401 is not idle: {}", text(&unauth));
}

#[test]
fn idle_watch_signals_the_entrypoint_with_reason_idle() {
    let w = Watch::new();
    let (url, mut agent) = fake_agent(w.tmp.path(), r#"{"last_activity_unix":1,"active_execs":0,"active_tunnels":0}"#);
    // Stands in for the entrypoint: records the TERM it gets.
    let target = Command::new("bash")
        .arg("-c")
        .arg("trap 'echo term > \"$1\"; exit 0' TERM; while :; do sleep 0.1; done")
        .arg("t")
        .arg(w.tmp.path().join("got"))
        .spawn()
        .unwrap();
    let mut target = target;
    let out = w.cmd(&url, target.id()).output().unwrap();
    let status = target.wait().unwrap();
    agent.kill().ok();
    assert!(out.status.success(), "{}", text(&out));
    assert!(status.success());
    assert_eq!(fs::read_to_string(w.tmp.path().join("got")).unwrap().trim(), "term");
    assert_eq!(fs::read_to_string(w.tmp.path().join("reason")).unwrap().trim(), "idle");
}

// ------------------------------------------------------- stop (SIGTERM) path

#[test]
fn aws_stop_runs_the_wip_push_with_the_recorded_reason_once() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("calls");
    let fake = write_script(
        tmp.path(),
        "triple-c-wip-push",
        &format!("#!/bin/bash\necho \"$*\" >> '{}'\n", log.display()),
    );
    let reason = tmp.path().join("reason");
    let blocks = aws_blocks(&["aws_env", "aws_stop"])
        .replace("/usr/local/bin/triple-c-wip-push", fake.to_str().unwrap())
        .replace("/run/triple-c/stop-reason", reason.to_str().unwrap());
    let script = format!("{blocks}\naws_before_stop\naws_before_stop\n");

    let out = bash(&script, &[], &[("TRIPLEC_HOST", "aws"), ("TRIPLEC_WIP_PUSH", "1")], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_to_string(&log).unwrap(), "--reason sigterm\n");

    fs::remove_file(&log).unwrap();
    fs::write(&reason, "idle\n").unwrap();
    let out = bash(&script, &[], &[("TRIPLEC_HOST", "aws"), ("TRIPLEC_WIP_PUSH", "1")], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_to_string(&log).unwrap(), "--reason idle\n");

    fs::remove_file(&log).unwrap();
    let out = bash(&script, &[], &[("TRIPLEC_HOST", "aws"), ("TRIPLEC_WIP_PUSH", "0")], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!log.exists(), "WIP push off");
}

#[test]
fn docker_mode_never_reaches_the_aws_blocks() {
    // Every new step sits behind TRIPLEC_HOST=aws; the scripts are only
    // started from those branches.
    for name in ["aws_env", "aws_home", "aws_repos", "aws_stop"] {
        let b = block(ENTRYPOINT, name);
        assert!(b.contains("aws"), "{name}");
    }
    let outside: String = {
        let mut s = ENTRYPOINT.to_string();
        for name in ["aws_env", "aws_home", "aws_repos", "aws_stop"] {
            s = s.replace(block(ENTRYPOINT, name), "");
        }
        s
    };
    let lines: Vec<&str> = outside.lines().collect();
    let calls = [
        "triple-c-wip-push",
        "triple-c-idle-watch",
        "clone_repos",
        "link_persistent_home",
        "aws_before_stop",
        "start_idle_watch",
    ];
    let guarded = |s: &str| s.contains("\"$triplec_aws\" = 1");
    let mut seen = 0;
    for (i, line) in lines.iter().enumerate() {
        let l = line.trim_start();
        if l.starts_with('#') || !calls.iter().any(|c| l.contains(c)) {
            continue;
        }
        seen += 1;
        // Guarded on the line itself or by the `if` right above it.
        assert!(
            guarded(l) || (i > 0 && lines[i - 1].trim_start().starts_with("if ") && guarded(lines[i - 1])),
            "unguarded call outside the aws blocks: {line}"
        );
    }
    assert_eq!(seen, 4, "the four call sites: Hybrid link, clone, idle watch, stop");
    // And the snapshot-sensitive variables are gone before the agent's
    // environment snapshot is taken.
    assert!(ENTRYPOINT.find("unset TRIPLEC_FILES").unwrap() < ENTRYPOINT.find("# >>> agent_token").unwrap());
    assert!(ENTRYPOINT.find("unset TRIPLEC_REPOS").unwrap() < ENTRYPOINT.find("# >>> agent_token").unwrap());
}
