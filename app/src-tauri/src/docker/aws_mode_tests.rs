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
/// The functions `entrypoint.sh` and `triple-c-apply-connect` both source.
const LIB: &str = include_str!("../../../../container/entrypoint-lib.sh");
const APPLY_CONNECT: &str = include_str!("../../../../container/triple-c-apply-connect");
const WIP_PUSH: &str = include_str!("../../../../container/triple-c-wip-push");
const IDLE_WATCH: &str = include_str!("../../../../container/triple-c-idle-watch");
const SCHEDULER: &str = include_str!("../../../../container/triple-c-scheduler");
const WORKSPACE_STATUS: &str = include_str!("../../../../container/triple-c-workspace-status");

/// A marked block of `entrypoint.sh` or, failing that, of the library it
/// sources.
fn eblock(name: &str) -> &'static str {
    if ENTRYPOINT.contains(&format!("# >>> {name}\n")) {
        block(ENTRYPOINT, name)
    } else {
        block(LIB, name)
    }
}

fn block<'a>(text: &'a str, name: &str) -> &'a str {
    let start = text
        .find(&format!("# >>> {name}\n"))
        .unwrap_or_else(|| panic!("start marker {name}"));
    let end = text
        .find(&format!("# <<< {name}\n"))
        .unwrap_or_else(|| panic!("end marker {name}"));
    &text[start..end]
}

/// The aws blocks (after the library's shared settings, `aws_lib`, which the
/// entrypoint sources first) with the container user switched off: no
/// `su claude`, no chown to a user the test host may not have.
fn aws_blocks(names: &[&str]) -> String {
    std::iter::once("aws_lib")
        .chain(names.iter().copied())
        .map(|n| eblock(n).replace("triplec_run_as=claude", "triplec_run_as="))
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
         printf '%s' \"${{triplec_repos_json:-}}\" > \"$1/repos\"\n\
         printf '%s' \"${{triplec_setup_script:-}}\" > \"$1/setup\"\n"
    )
}

#[test]
fn aws_files_decode_to_the_host_dirs_mode_600_and_leave_the_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let files = format!(
        r#"{{"ssh/id_ed25519":"{}","ssh/config":"{}","ca/corp.crt":"{}","ssh/..":"{}","ssh/a/b":"{}","ssh/":"{}","home/x":"{}","ca/bad":"%%%","ssh/{long}":"{}","ssh/\ud800":"{}"}}"#,
        b64("PRIVATE-KEY-BODY"),
        b64("Host *\n"),
        b64("-----BEGIN CERTIFICATE-----"),
        b64("x"),
        b64("x"),
        b64("x"),
        b64("x"),
        b64("x"),
        b64("x"),
        long = "a".repeat(300),
    );
    let repos = r#"[{"url":"https://example.invalid/r.git","branch":null,"folder":"r"}]"#;
    let out = bash(
        &files_script(tmp.path()),
        &[tmp.path()],
        &[
            ("TRIPLEC_HOST", "aws"),
            ("TRIPLEC_FILES", &files),
            ("TRIPLEC_REPOS", repos),
            ("TRIPLEC_SETUP_SCRIPT", "echo SETUP-BODY"),
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
    let long_key = format!("ssh/{}", "a".repeat(300));
    for (key, why) in [
        ("'ssh/..'", "not ssh/<name> or ca/<name>"),
        ("'ssh/a/b'", "not ssh/<name> or ca/<name>"),
        ("'ssh/'", "not ssh/<name> or ca/<name>"),
        ("'home/x'", "not ssh/<name> or ca/<name>"),
        ("'ca/bad'", "value is not base64"),
        (&format!("'{long_key}'"), "File name too long"),
        ("'ssh/\\ud800'", "UnicodeEncodeError"),
    ] {
        let line = format!("entrypoint: refusing file entry {key}: {why}");
        assert!(log.lines().any(|l| l == line), "missing `{line}` in:\n{log}");
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
    // So does the setup script, for aws_prepare_run to write to its file.
    assert!(!env.contains("TRIPLEC_SETUP_SCRIPT=") && !env.contains("SETUP-BODY"), "{env}");
    assert!(!agent_env.contains("TRIPLEC_SETUP_SCRIPT") && !agent_env.contains("SETUP-BODY"));
    assert_eq!(fs::read_to_string(tmp.path().join("setup")).unwrap(), "echo SETUP-BODY");
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
fn efs_storage_links_home_state_like_hybrid() {
    let script = format!(
        "{}\nfor m in hybrid efs ephemeral '' HYBRID efsx; do aws_storage_links_home \"$m\" && echo \"$m:yes\" || echo \"$m:no\"; done\n",
        aws_blocks(&["aws_env", "aws_home"])
    );
    let out = bash(&script, &[], &[], &[]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hybrid:yes\nefs:yes\nephemeral:no\n:no\nHYBRID:no\nefsx:no\n"
    );
    // The call site asks it, rather than testing for `hybrid` alone (an EFS
    // task would otherwise get no home links at all).
    let call = ENTRYPOINT
        .lines()
        .position(|l| l.trim_start().starts_with("link_persistent_home /home/claude"))
        .unwrap();
    let above = ENTRYPOINT.lines().nth(call - 1).unwrap();
    assert_eq!(
        above.trim(),
        "if [ \"$triplec_aws\" = 1 ] && aws_storage_links_home \"${TRIPLEC_STORAGE:-}\"; then"
    );
}

#[test]
fn efs_workspace_check_warns_only_when_workspace_is_not_mounted() {
    let tmp = tempfile::tempdir().unwrap();
    let script = format!("{}\naws_efs_workspace_check \"$1\"; echo \"rc=$?\"\n", aws_blocks(&["aws_env", "aws_home"]));
    let out = bash(&script, &[tmp.path()], &[], &[]);
    assert!(text(&out).contains("is not mounted"), "{}", text(&out));
    assert!(text(&out).contains("rc=0"), "{}", text(&out));
    let out = bash(&script, &[Path::new("/proc")], &[], &[]);
    assert!(!text(&out).contains("warning"), "{}", text(&out));
    assert!(text(&out).contains("rc=0"), "{}", text(&out));
}

/// `write_claude_json` with the Hybrid mount at `<tmp>/mnt`, in aws mode or not.
fn write_claude_json(tmp: &Path, aws: bool) -> Output {
    let mnt = tmp.join("mnt");
    let blocks = aws_blocks(&["aws_env", "write_claude_json"])
        .replace("triplec_home_mnt=/mnt/triple-c-home", &format!("triplec_home_mnt='{}'", mnt.display()));
    // chown to a user the host may not have is noise here, not the point.
    let script = format!(
        "chown() {{ :; }}\n{blocks}\ntriplec_aws={}\nCLAUDE_JSON=\"$1/.claude.json\"\nwrite_claude_json '{{\"a\":1}}'\n",
        if aws { 1 } else { 0 }
    );
    bash(&script, &[&tmp.join("home")], &[], &[])
}

fn hybrid_dirs(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let (home, mnt) = (tmp.join("home"), tmp.join("mnt"));
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&mnt).unwrap();
    (home, mnt)
}

#[test]
fn write_claude_json_keeps_a_linked_claude_json_linked() {
    let tmp = tempfile::tempdir().unwrap();
    let (home, mnt) = hybrid_dirs(tmp.path());
    fs::write(mnt.join(".claude.json"), "{}").unwrap();
    std::os::unix::fs::symlink(mnt.join(".claude.json"), home.join(".claude.json")).unwrap();
    let out = write_claude_json(tmp.path(), true);
    assert!(out.status.success(), "{}", text(&out));
    assert!(fs::symlink_metadata(home.join(".claude.json")).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_to_string(mnt.join(".claude.json")).unwrap().trim(), "{\"a\":1}");
    assert_eq!(mode(&mnt.join(".claude.json")), 0o600);
    assert!(!mnt.join(".claude.json.triple-c-tmp").exists());
}

#[test]
fn write_claude_json_refuses_a_link_triple_c_did_not_make() {
    let tmp = tempfile::tempdir().unwrap();
    let (home, mnt) = hybrid_dirs(tmp.path());
    // A link pointing somewhere else entirely.
    let elsewhere = tmp.path().join("elsewhere");
    fs::write(&elsewhere, "untouched").unwrap();
    std::os::unix::fs::symlink(&elsewhere, home.join(".claude.json")).unwrap();
    let out = write_claude_json(tmp.path(), true);
    assert!(!out.status.success());
    assert!(text(&out).contains("did not make"), "{}", text(&out));
    assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "untouched");

    // The right link, but its target swapped for a link of its own.
    fs::remove_file(home.join(".claude.json")).unwrap();
    std::os::unix::fs::symlink(mnt.join(".claude.json"), home.join(".claude.json")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, mnt.join(".claude.json")).unwrap();
    let out = write_claude_json(tmp.path(), true);
    assert!(!out.status.success());
    assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "untouched");
}

#[test]
fn write_claude_json_outside_aws_writes_a_plain_file_as_before() {
    let tmp = tempfile::tempdir().unwrap();
    let (home, _mnt) = hybrid_dirs(tmp.path());
    let out = write_claude_json(tmp.path(), false);
    assert!(out.status.success(), "{}", text(&out));
    assert!(fs::symlink_metadata(home.join(".claude.json")).unwrap().is_file());
    assert_eq!(fs::read_to_string(home.join(".claude.json")).unwrap().trim(), "{\"a\":1}");
}

#[test]
fn the_hybrid_ssh_dir_is_used_only_through_the_link_triple_c_made() {
    let tmp = tempfile::tempdir().unwrap();
    let (home, mnt) = hybrid_dirs(tmp.path());
    let check = |home: &Path| {
        let blocks = aws_blocks(&["aws_env", "aws_home"])
            .replace("triplec_home_mnt=/mnt/triple-c-home", &format!("triplec_home_mnt='{}'", mnt.display()));
        bash(&format!("{blocks}\naws_hybrid_ssh_dir \"$1\""), &[home], &[], &[])
    };
    fs::create_dir_all(mnt.join(".ssh")).unwrap();
    std::os::unix::fs::symlink(mnt.join(".ssh"), home.join(".ssh")).unwrap();
    let out = check(&home);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), mnt.join(".ssh").to_str().unwrap());

    // Persisted .ssh replaced by a link (to /etc, say): refused.
    fs::remove_dir(mnt.join(".ssh")).unwrap();
    std::os::unix::fs::symlink(tmp.path(), mnt.join(".ssh")).unwrap();
    assert!(!check(&home).status.success());

    // ~/.ssh linked anywhere else: refused.
    fs::remove_file(home.join(".ssh")).unwrap();
    std::os::unix::fs::symlink(tmp.path(), home.join(".ssh")).unwrap();
    assert!(!check(&home).status.success());
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
            {{"url":"{r}","branch":null,"folder":"../escape"}},
            {{"url":"{r}","branch":null,"folder":"bad\ud800"}},
            {{"url":"{r}","branch":null,"folder":".dotted"}}]"#,
        r = remote.display()
    );
    let status = tmp.path().join("status");
    fs::create_dir_all(&status).unwrap();
    let script = format!(
        "{}\nclone_repos \"$JSON\" \"$1\" \"$2\"\necho \"rc=$?\"\n",
        aws_blocks(&["aws_env", "aws_repos"])
    );
    let out = bash(&script, &[&ws, &status], &[("JSON", &json)], &[]);
    let log = text(&out);
    assert!(out.status.success(), "{log}");
    assert!(log.contains("rc=0"), "{log}");

    assert_eq!(fs::read_to_string(ws.join("main-copy/README")).unwrap(), "hello");
    assert_eq!(git(&ws.join("dev-copy"), &["rev-parse", "--abbrev-ref", "HEAD"]), "dev");
    assert!(log.contains("Triple-C: clone of broken failed:"), "{log}");
    assert!(!log.contains("clone of existing failed"), "{log}");
    assert!(log.contains("../escape"), "a bad folder is refused by name: {log}");
    assert!(log.contains("bad folder 'bad\\ud800'"), "{log}");
    assert!(!tmp.path().join("escape").exists());
    assert_eq!(fs::read_to_string(ws.join(".dotted/README")).unwrap(), "hello");
    assert_eq!(
        fs::read_to_string(status.join("repos")).unwrap(),
        "main-copy\tcloning\nmain-copy\tcloned\ndev-copy\tcloning\ndev-copy\tcloned\n\
         existing\texists\nbroken\tcloning\nbroken\tfailed\n.dotted\tcloning\n.dotted\tcloned\n"
    );
}

/// clone_repos over `ws` (persistent, as with EFS storage), its status in `status`.
fn clone_into(ws: &Path, status: &Path, json: &str) -> (String, String) {
    fs::create_dir_all(status).unwrap();
    let script = format!("{}\nclone_repos \"$JSON\" \"$1\" \"$2\"\n", aws_blocks(&["aws_env", "aws_repos"]));
    let out = bash(&script, &[ws, status], &[("JSON", json)], &[]);
    assert!(out.status.success(), "{}", text(&out));
    (text(&out), fs::read_to_string(status.join("repos")).unwrap_or_default())
}

fn repos_json(remote: &Path, folders: &[&str]) -> String {
    let items: Vec<String> = folders
        .iter()
        .map(|f| format!(r#"{{"url":"{}","branch":null,"folder":"{f}"}}"#, remote.display()))
        .collect();
    format!("[{}]", items.join(","))
}

#[test]
fn clone_repos_clones_through_scratch_and_redoes_a_clone_a_killed_task_left_there() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    fs::create_dir_all(&ws).unwrap();
    let remote = bare_remote(tmp.path(), "proj");
    let json = repos_json(&remote, &["a"]);
    // The last task was killed mid-clone: only scratch holds it.
    let scratch = ws.join(".triple-c-clone");
    fs::create_dir_all(scratch.join("a/.git")).unwrap();
    fs::write(scratch.join("a/half"), "x").unwrap();
    let (_, status) = clone_into(&ws, &tmp.path().join("s1"), &json);
    assert_eq!(status, "a\tcloning\na\tcloned\n");
    assert_eq!(fs::read_to_string(ws.join("a/README")).unwrap(), "hello");
    assert!(!ws.join("a/half").exists());
    // Nothing is left in scratch after a successful run.
    assert_eq!(fs::read_dir(&scratch).map(|d| d.count()).unwrap_or(0), 0);
}

#[test]
fn clone_repos_never_deletes_a_folder_at_its_final_path() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    fs::create_dir_all(&ws).unwrap();
    let remote = bare_remote(tmp.path(), "proj");
    // A repository already in place, the user's work in it, and a stale
    // scratch copy of the same name.
    git(&ws, &["clone", "-q", remote.to_str().unwrap(), "a"]);
    fs::write(ws.join("a/mine"), "work").unwrap();
    fs::create_dir_all(ws.join(".triple-c-clone/a/.git")).unwrap();
    // A folder the user made that is not a repository, and a clone that
    // fails into it.
    fs::create_dir_all(ws.join("notes")).unwrap();
    fs::write(ws.join("notes/todo"), "keep").unwrap();
    // An empty folder is cloned into, as `git clone` would.
    fs::create_dir_all(ws.join("empty")).unwrap();
    let json = format!(
        r#"[{{"url":"{r}","branch":null,"folder":"a"}},
            {{"url":"{r}","branch":null,"folder":"empty"}},
            {{"url":"{r}","branch":null,"folder":"notes"}},
            {{"url":"/nonexistent/nope.git","branch":null,"folder":"gone"}}]"#,
        r = remote.display()
    );
    for run in 0..3 {
        let (log, status) = clone_into(&ws, &tmp.path().join(format!("s{run}")), &json);
        assert!(status.starts_with("a\texists\n"), "{status}");
        assert!(status.contains("notes\tfailed\n"), "{status}: {log}");
        assert!(status.contains("gone\tfailed\n"), "{status}");
        assert_eq!(fs::read_to_string(ws.join("a/mine")).unwrap(), "work", "run {run}");
        assert_eq!(fs::read_to_string(ws.join("notes/todo")).unwrap(), "keep", "run {run}");
        assert!(!ws.join("notes/.git").exists());
        assert!(!ws.join("gone").exists(), "a failed clone writes nothing at its final path");
        assert_eq!(fs::read_to_string(ws.join("empty/README")).unwrap(), "hello");
        let want = if run == 0 { "empty\tcloning\nempty\tcloned\n" } else { "empty\texists\n" };
        assert!(status.contains(want), "{status}");
    }
}

#[test]
fn clone_repos_refuses_the_scratch_folder_name() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    fs::create_dir_all(&ws).unwrap();
    let remote = bare_remote(tmp.path(), "proj");
    let (log, status) = clone_into(&ws, &tmp.path().join("s"), &repos_json(&remote, &[".triple-c-clone"]));
    assert!(log.contains("bad folder '.triple-c-clone'"), "{log}");
    assert_eq!(status, "");
}

/// A stand-in `ssh` that records how it was called and fails.
fn fake_ssh(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let log = dir.join("ssh-calls");
    let ssh = write_script(dir, "fake-ssh", &format!("#!/bin/sh\necho \"$*\" >> '{}'\nexit 255\n", log.display()));
    (ssh, log)
}

#[test]
fn clone_repos_keeps_the_users_ssh_command_and_adds_batch_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let (ssh, calls) = fake_ssh(tmp.path());
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join(".gitconfig"), format!("[core]\n\tsshCommand = sh {} -o User=me\n", ssh.display())).unwrap();
    let ws = tmp.path().join("ws");
    fs::create_dir_all(&ws).unwrap();
    let json = r#"[{"url":"ssh://git@example.invalid/r.git","branch":null,"folder":"r"}]"#;
    let script = format!("{}\nclone_repos \"$JSON\" \"$1\"\n", aws_blocks(&["aws_env", "aws_repos"]));
    let out = bash(
        &script,
        &[&ws],
        &[("JSON", json), ("HOME", home.to_str().unwrap()), ("GIT_CONFIG_NOSYSTEM", "1")],
        &["GIT_SSH_COMMAND", "GIT_SSH"],
    );
    assert!(text(&out).contains("clone of r failed"), "{}", text(&out));
    let calls = fs::read_to_string(&calls).unwrap();
    assert!(calls.contains("-o User=me -o BatchMode=yes"), "{calls}");
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
    wip_push_with_clone_dir(ws, args, &tmp.join("no-clone-status"))
}

fn wip_push_with_clone_dir(ws: &Path, args: &[&str], clone_dir: &Path) -> Output {
    let tmp = ws.parent().unwrap();
    let script = write_script(tmp, "triple-c-wip-push", WIP_PUSH);
    // Through bash rather than exec'd: a script written a moment ago can be
    // ETXTBSY to exec while a parallel test's fork still holds it open.
    Command::new("bash")
        .arg(script)
        .args(args)
        .env("TRIPLEC_WORKSPACE_ROOT", ws)
        .env("TRIPLEC_CLONE_STATUS_DIR", clone_dir)
        .env("TRIPLEC_RUN_ID_FILE", tmp.join("no-run-id"))
        .env("TRIPLEC_WIP_LAST_DIR", tmp.join("no-wip-last"))
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
        name.len() == 20
            && name.as_bytes()[8] == b'-'
            && name[..15].chars().filter(|c| c.is_ascii_digit()).count() == 14
            && name.ends_with("-proj"),
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
fn wip_push_covers_dot_folders() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _repo, remote) = workspace_with_clone(tmp.path());
    git(&ws, &["clone", "-q", remote.to_str().unwrap(), ".hidden"]);
    fs::write(ws.join(".hidden/y"), "y").unwrap();
    let out = wip_push(&ws, &["--dry-run"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains(r#""folder":".hidden""#), "{}", text(&out));
    let out = wip_push(&ws, &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(wip_refs(&remote).len(), 1);
}

#[test]
fn wip_push_ignores_the_clone_scratch_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _repo, remote) = workspace_with_clone(tmp.path());
    // A dirty clone left in scratch (and scratch itself made a repository):
    // neither is the workspace's own.
    let scratch = ws.join(".triple-c-clone");
    fs::create_dir_all(&scratch).unwrap();
    git(&scratch, &["clone", "-q", remote.to_str().unwrap(), "half"]);
    fs::write(scratch.join("half/y"), "y").unwrap();
    git(&scratch, &["init", "-q"]);
    let out = wip_push(&ws, &["--dry-run"]);
    let dry = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(!dry.contains("triple-c-clone") && !dry.contains("half"), "{dry}");
    let out = wip_push(&ws, &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(wip_refs(&remote).is_empty());
}

#[test]
fn wip_push_keeps_the_repos_ssh_command_and_adds_batch_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, repo, _remote) = workspace_with_clone(tmp.path());
    let (ssh, calls) = fake_ssh(tmp.path());
    git(&repo, &["config", "core.sshCommand", &format!("sh {} -o User=me", ssh.display())]);
    git(&repo, &["remote", "set-url", "origin", "ssh://git@example.invalid/r.git"]);
    fs::write(repo.join("y"), "y").unwrap();
    let out = wip_push(&ws, &[]);
    assert!(!out.status.success(), "the fake ssh fails the push");
    let calls = fs::read_to_string(&calls).unwrap();
    assert!(calls.contains("-o User=me -o BatchMode=yes"), "{calls}");
}

#[test]
fn wip_push_uploads_lfs_objects_before_the_no_verify_push() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, repo, remote) = workspace_with_clone(tmp.path());
    // A stand-in git-lfs: present, reports one LFS file, records the push.
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let log = tmp.path().join("lfs-calls");
    write_script(
        &bin,
        "git-lfs",
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n  version) echo fake;;\n  ls-files) echo big.bin;;\n  push) echo \"$*\" >> '{}';;\nesac\n",
            log.display()
        ),
    );
    fs::write(repo.join("y"), "y").unwrap();
    let script = write_script(tmp.path(), "triple-c-wip-push", WIP_PUSH);
    let out = Command::new("bash")
        .arg(script)
        .env("TRIPLEC_WORKSPACE_ROOT", &ws)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", tmp.path())
        .env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    let refs = wip_refs(&remote);
    let pushed = git(&remote, &["rev-parse", &refs[0]]);
    assert_eq!(fs::read_to_string(&log).unwrap().trim(), format!("push origin {pushed}"));
}

#[test]
fn wip_push_skips_a_repo_the_background_clone_is_still_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, repo, remote) = workspace_with_clone(tmp.path());
    // `half` is mid-clone: a checkout in progress reads as a dirty tree.
    git(&ws, &["clone", "-q", remote.to_str().unwrap(), "half"]);
    fs::remove_file(ws.join("half/README")).unwrap();
    fs::write(repo.join("y"), "y").unwrap();
    let clone = tmp.path().join("clone");
    fs::create_dir_all(&clone).unwrap();
    fs::write(clone.join("repos"), "proj\tcloning\nproj\tcloned\nhalf\tcloning\n").unwrap();

    let out = wip_push_with_clone_dir(&ws, &["--dry-run"], &clone);
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<serde_json::Value> = stdout.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let by = |f: &str| lines.iter().find(|v| v["folder"] == f).unwrap().clone();
    assert_eq!(
        by("half"),
        serde_json::json!({"folder": "half", "dirty": false, "unpushed": 0, "branch": "", "cloning": true})
    );
    assert_eq!(by("proj")["dirty"], true, "a finished clone is checked as usual");
    // The Rust side parses that line.
    let parsed = crate::commands::stop_guard::parse_dry_run(&stdout).unwrap();
    assert!(parsed.iter().any(|r| r.folder == "half" && r.cloning));

    let out = wip_push_with_clone_dir(&ws, &[], &clone);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("half: still cloning"), "{}", text(&out));
    assert_eq!(wip_refs(&remote).len(), 1, "only proj was pushed");

    // Once the clone is done, the folder is an ordinary repository again.
    fs::write(clone.join("done"), "").unwrap();
    let out = wip_push_with_clone_dir(&ws, &["--dry-run"], &clone);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(r#"{"folder":"half","dirty":true"#), "{stdout}");
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
    // now, last activity, claude running, jobs running, minutes.
    assert!(is_idle(&["10000", "9400", "0", "0", "10"]), "exactly N minutes");
    assert!(is_idle(&["10000", "1000", "0", "0", "10"]));
    assert!(!is_idle(&["10000", "9401", "0", "0", "10"]), "one second short");
    assert!(!is_idle(&["10000", "1000", "1", "0", "10"]), "claude is running");
    assert!(!is_idle(&["10000", "1000", "0", "1", "10"]), "a scheduler job runs");
    // Anything unreadable is "not idle".
    assert!(!is_idle(&["10000", "", "0", "0", "10"]));
    assert!(!is_idle(&["10000", "x", "0", "0", "10"]));
    assert!(!is_idle(&["10000", "1000", "", "0", "10"]));
    assert!(!is_idle(&["10000", "1000", "0", "0", "0"]), "0 minutes is off");
    assert!(!is_idle(&["10000", "1000", "0", "0", ""]));
    // The old arity (with exec/tunnel counters) is refused, not misread.
    assert!(!is_idle(&["10000", "1000", "0", "0", "0", "0", "10"]));
}

struct Watch {
    tmp: tempfile::TempDir,
}

impl Watch {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        write_script(tmp.path(), "triple-c-idle-watch", IDLE_WATCH);
        write_script(tmp.path(), "triple-c-scheduler", SCHEDULER);
        // A stand-in WIP push: `--dry-run` prints the file `dry` (nothing:
        // clean repos), a push records its arguments in `pushes` and exits
        // with the number in `push_rc` (0 when absent).
        let d = tmp.path().display();
        write_script(
            tmp.path(),
            "triple-c-wip-push",
            &format!(
                "#!/bin/bash\nif [ \"$1\" = --dry-run ]; then cat '{d}/dry' 2>/dev/null; exit 0; fi\n\
                 echo \"$*\" >> '{d}/pushes'\nexit \"$(cat '{d}/push_rc' 2>/dev/null || echo 0)\"\n"
            ),
        );
        fs::write(tmp.path().join("token"), "test-token-not-real").unwrap();
        fs::create_dir_all(tmp.path().join("home")).unwrap();
        Watch { tmp }
    }

    fn cmd(&self, agent_url: &str, signal_pid: u32) -> Command {
        let p = self.tmp.path();
        let mut c = Command::new("bash");
        c.arg(p.join("triple-c-idle-watch"));
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
            .env("TRIPLEC_WIP_PUSH_BIN", p.join("triple-c-wip-push"))
            // The test host may well be running a `claude` of its own.
            .env("TRIPLEC_IDLE_PROC_NAME", "triple-c-no-such-process");
        c
    }

    fn dirty(&self) {
        fs::write(
            self.tmp.path().join("dry"),
            "{\"folder\":\"api\",\"dirty\":true,\"unpushed\":0,\"branch\":\"main\"}\n\
             {\"folder\":\"web\",\"dirty\":false,\"unpushed\":0,\"branch\":\"main\"}\n",
        )
        .unwrap();
    }

    fn pushes(&self) -> String {
        fs::read_to_string(self.tmp.path().join("pushes")).unwrap_or_default()
    }
}

/// Stands in for the entrypoint: records the TERM it gets in `got`.
fn fake_entrypoint(dir: &Path) -> std::process::Child {
    Command::new("bash")
        .arg("-c")
        .arg("trap 'echo term > \"$1\"; exit 0' TERM; while :; do sleep 0.1; done")
        .arg("t")
        .arg(dir.join("got"))
        .spawn()
        .unwrap()
}

/// Run the watcher loop for at most `secs`; `None` when it was still running
/// (it had not signalled) and was killed. Never hangs a test run.
fn run_watch_for(mut cmd: Command, secs: u64) -> Option<Output> {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return Some(child.wait_with_output().unwrap());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    child.kill().ok();
    let out = child.wait_with_output().unwrap();
    eprintln!("watcher (killed): {}", text(&out));
    None
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

const OLD_ACTIVITY: &str = r#"{"last_activity_unix":1,"active_execs":0,"active_tunnels":0}"#;

#[test]
fn idle_watch_check_counts_an_unfinished_clone_as_busy() {
    let w = Watch::new();
    let (url, mut agent) = fake_agent(w.tmp.path(), OLD_ACTIVITY);
    let clone = w.tmp.path().join("clone");
    fs::create_dir_all(&clone).unwrap();
    let busy = w.cmd(&url, 0).env("TRIPLEC_CLONE_STATUS_DIR", &clone).arg("--check").output().unwrap();
    fs::write(clone.join("done"), "").unwrap();
    let idle = w.cmd(&url, 0).env("TRIPLEC_CLONE_STATUS_DIR", &clone).arg("--check").output().unwrap();
    agent.kill().ok();
    assert_eq!(busy.status.code(), Some(1), "{}", text(&busy));
    assert_eq!(idle.status.code(), Some(0), "{}", text(&idle));
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
    let (url, mut agent) = fake_agent(w.tmp.path(), OLD_ACTIVITY);
    let idle = w.cmd(&url, 0).arg("--check").output().unwrap();
    let recent = format!(
        r#"{{"last_activity_unix":{},"active_execs":0,"active_tunnels":0}}"#,
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
    );
    let (busy_url, mut busy_agent) = fake_agent(w.tmp.path(), &recent);
    let busy = w.cmd(&busy_url, 0).arg("--check").output().unwrap();
    fs::write(w.tmp.path().join("token"), "wrong").unwrap();
    let unauth = w.cmd(&url, 0).arg("--check").output().unwrap();
    agent.kill().ok();
    busy_agent.kill().ok();
    assert_eq!(idle.status.code(), Some(0), "{}", text(&idle));
    assert_eq!(busy.status.code(), Some(1), "recent activity: {}", text(&busy));
    assert_eq!(unauth.status.code(), Some(1), "a 401 is not idle: {}", text(&unauth));
}

#[test]
fn idle_watch_ignores_open_but_silent_execs_and_tunnels() {
    // R11: an open terminal tab (an attach) or a tunnel no longer keeps the
    // workspace up; only their traffic moves the clock.
    let w = Watch::new();
    let (url, mut agent) =
        fake_agent(w.tmp.path(), r#"{"last_activity_unix":1,"active_execs":3,"active_tunnels":2}"#);
    let out = w.cmd(&url, 0).arg("--check").output().unwrap();
    agent.kill().ok();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
}

#[test]
fn idle_watch_check_with_unsaved_work_depends_on_the_wip_push() {
    let w = Watch::new();
    w.dirty();
    let (url, mut agent) = fake_agent(w.tmp.path(), OLD_ACTIVITY);
    let off = w.cmd(&url, 0).env("TRIPLEC_WIP_PUSH", "0").arg("--check").output().unwrap();
    let on = w.cmd(&url, 0).env("TRIPLEC_WIP_PUSH", "1").arg("--check").output().unwrap();
    // A dry run that fails is a doubt: not idle.
    write_script(w.tmp.path(), "triple-c-wip-push", "#!/bin/bash\nexit 1\n");
    let unknown = w.cmd(&url, 0).env("TRIPLEC_WIP_PUSH", "1").arg("--check").output().unwrap();
    agent.kill().ok();
    assert_eq!(off.status.code(), Some(1), "unsaved work, WIP push off: {}", text(&off));
    assert_eq!(on.status.code(), Some(0), "the WIP push would take it: {}", text(&on));
    assert_eq!(unknown.status.code(), Some(1), "{}", text(&unknown));
    assert_eq!(w.pushes(), "", "--check never pushes");
}

#[test]
fn idle_watch_signals_the_entrypoint_with_reason_idle() {
    let w = Watch::new();
    let (url, mut agent) = fake_agent(w.tmp.path(), OLD_ACTIVITY);
    let mut target = fake_entrypoint(w.tmp.path());
    let out = run_watch_for(w.cmd(&url, target.id()), 20).expect("it signals");
    let status = target.wait().unwrap();
    agent.kill().ok();
    assert!(out.status.success(), "{}", text(&out));
    assert!(status.success());
    assert_eq!(fs::read_to_string(w.tmp.path().join("got")).unwrap().trim(), "term");
    assert_eq!(fs::read_to_string(w.tmp.path().join("reason")).unwrap().trim(), "idle");
    assert_eq!(w.pushes(), "", "nothing unsaved, nothing pushed");
}

#[test]
fn idle_watch_with_unsaved_work_and_the_wip_push_off_stays_up() {
    let w = Watch::new();
    w.dirty();
    let (url, mut agent) = fake_agent(w.tmp.path(), OLD_ACTIVITY);
    let mut target = fake_entrypoint(w.tmp.path());
    let mut cmd = w.cmd(&url, target.id());
    cmd.env("TRIPLEC_WIP_PUSH", "0");
    let ran = run_watch_for(cmd, 4);
    target.kill().ok();
    target.wait().ok();
    agent.kill().ok();
    assert!(ran.is_none(), "it must not stop: {:?}", ran.map(|o| text(&o)));
    assert!(!w.tmp.path().join("got").exists(), "no TERM");
    assert!(!w.tmp.path().join("reason").exists(), "no marker");
    assert_eq!(w.pushes(), "");
}

#[test]
fn idle_watch_pushes_before_an_idle_stop_and_stays_up_while_the_push_fails() {
    // The push fails: the workspace stays up and retries, backing off.
    let w = Watch::new();
    w.dirty();
    fs::write(w.tmp.path().join("push_rc"), "1").unwrap();
    let (url, mut agent) = fake_agent(w.tmp.path(), OLD_ACTIVITY);
    let mut target = fake_entrypoint(w.tmp.path());
    let mut cmd = w.cmd(&url, target.id());
    cmd.env("TRIPLEC_WIP_PUSH", "1").env("TRIPLEC_WIP_BACKOFF_MAX", "2");
    let ran = run_watch_for(cmd, 6);
    assert!(ran.is_none(), "a failed push must not stop the workspace");
    assert!(!w.tmp.path().join("got").exists(), "no TERM");
    assert!(!w.tmp.path().join("reason").exists(), "no marker");
    let tries = w.pushes().lines().count();
    assert!((1..=3).contains(&tries), "retried with a back-off, not every second: {tries}");
    assert!(w.pushes().lines().all(|l| l == "--reason idle"), "{}", w.pushes());

    // Now the push works: it pushes once, then signals with the idle marker.
    fs::write(w.tmp.path().join("push_rc"), "0").unwrap();
    fs::remove_file(w.tmp.path().join("pushes")).unwrap();
    let mut cmd = w.cmd(&url, target.id());
    cmd.env("TRIPLEC_WIP_PUSH", "1");
    let out = run_watch_for(cmd, 20).expect("it stops once the push succeeded");
    target.wait().unwrap();
    agent.kill().ok();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(w.pushes(), "--reason idle\n");
    assert_eq!(fs::read_to_string(w.tmp.path().join("got")).unwrap().trim(), "term");
    assert_eq!(fs::read_to_string(w.tmp.path().join("reason")).unwrap().trim(), "idle");
}

#[test]
fn idle_watch_goes_back_to_watching_when_someone_comes_back_during_the_push() {
    let w = Watch::new();
    w.dirty();
    let d = w.tmp.path();
    // The push starts a process the watcher counts as `claude` (a renamed
    // `sleep`, so `pgrep -x` matches it), as a user returning mid-push would.
    let busy_name = format!("tcbusy{}", std::process::id() % 100_000);
    let sleep = Command::new("sh").arg("-c").arg("command -v sleep").output().unwrap();
    let sleep = String::from_utf8_lossy(&sleep.stdout).trim().to_string();
    fs::copy(&sleep, d.join(&busy_name)).unwrap();
    fs::set_permissions(d.join(&busy_name), fs::Permissions::from_mode(0o755)).unwrap();
    write_script(
        d,
        "triple-c-wip-push",
        &format!(
            "#!/bin/bash\nif [ \"$1\" = --dry-run ]; then cat '{p}/dry'; exit 0; fi\n\
             echo \"$*\" >> '{p}/pushes'\n'{p}/{busy_name}' 30 </dev/null >/dev/null 2>&1 &\n\
             echo $! > '{p}/busy.pid'\nexit 0\n",
            p = d.display()
        ),
    );
    let (url, mut agent) = fake_agent(d, OLD_ACTIVITY);
    let mut target = fake_entrypoint(d);
    let mut cmd = w.cmd(&url, target.id());
    cmd.env("TRIPLEC_WIP_PUSH", "1").env("TRIPLEC_IDLE_PROC_NAME", &busy_name);
    let ran = run_watch_for(cmd, 5);
    if let Ok(pid) = fs::read_to_string(d.join("busy.pid")) {
        let _ = Command::new("kill").arg(pid.trim()).status();
    }
    target.kill().ok();
    target.wait().ok();
    agent.kill().ok();
    assert!(ran.is_none(), "it must not stop: {:?}", ran.map(|o| text(&o)));
    assert_eq!(w.pushes(), "--reason idle\n", "it pushed once, then saw the activity");
    assert!(!d.join("got").exists(), "no TERM");
    assert!(!d.join("reason").exists(), "no idle marker");
}

#[test]
fn idle_watch_keeps_a_stop_guard_marker_already_recorded() {
    for marker in ["pushed", "discard", "lifetime"] {
        let w = Watch::new();
        fs::write(w.tmp.path().join("reason"), format!("{marker}\n")).unwrap();
        let (url, mut agent) = fake_agent(w.tmp.path(), OLD_ACTIVITY);
        let mut target = Command::new("bash")
            .arg("-c")
            .arg("trap 'exit 0' TERM; while :; do sleep 0.1; done")
            .spawn()
            .unwrap();
        let out = run_watch_for(w.cmd(&url, target.id()), 20).expect("it signals");
        target.wait().unwrap();
        agent.kill().ok();
        assert!(out.status.success(), "{}", text(&out));
        assert_eq!(fs::read_to_string(w.tmp.path().join("reason")).unwrap().trim(), marker);
    }
}

// ------------------------------------------------------- stop (SIGTERM) path

#[test]
fn aws_stop_pushes_on_sigterm_once_and_leaves_an_idle_stop_to_the_watcher() {
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

    // An idle stop: the watcher already pushed, so the trap does not, and it
    // exits with the idle status the app reads from ECS.
    fs::remove_file(&log).unwrap();
    fs::write(&reason, "idle\n").unwrap();
    let idle_script = format!("{blocks}\naws_before_stop\necho \"status=${{triplec_exit_status:-0}}\"\n");
    let out = bash(&idle_script, &[], &[("TRIPLEC_HOST", "aws"), ("TRIPLEC_WIP_PUSH", "1")], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!log.exists(), "no second push after the watcher's");
    assert!(text(&out).contains("entrypoint: stopping (idle)"), "{}", text(&out));
    assert!(text(&out).contains("status=75"), "{}", text(&out));

    // A lifetime-cap stop: the watcher already tried its push; exit 76.
    fs::write(&reason, "lifetime\n").unwrap();
    let out = bash(&idle_script, &[], &[("TRIPLEC_HOST", "aws"), ("TRIPLEC_WIP_PUSH", "1")], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!log.exists(), "no second push after the watcher's");
    assert!(text(&out).contains("entrypoint: stopping (lifetime)"), "{}", text(&out));
    assert!(text(&out).contains("status=76"), "{}", text(&out));
    fs::remove_file(&reason).unwrap();

    let out = bash(&script, &[], &[("TRIPLEC_HOST", "aws"), ("TRIPLEC_WIP_PUSH", "0")], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!log.exists(), "WIP push off");
}

#[test]
fn aws_stop_skips_its_push_when_the_app_already_pushed_or_the_user_declined() {
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
    let script = format!("{blocks}\naws_before_stop\n");
    for marker in ["pushed", "discard"] {
        fs::write(&reason, format!("{marker}\n")).unwrap();
        let out = bash(&script, &[], &[("TRIPLEC_HOST", "aws"), ("TRIPLEC_WIP_PUSH", "1")], &[]);
        assert!(out.status.success(), "{}", text(&out));
        assert!(!log.exists(), "{marker}: no second push");
        assert!(text(&out).contains(&format!("already handled by Triple-C ({marker})")), "{}", text(&out));
    }
    // Anything else is not a marker the app writes: the push still runs.
    fs::write(&reason, "something-else\n").unwrap();
    let out = bash(&script, &[], &[("TRIPLEC_HOST", "aws"), ("TRIPLEC_WIP_PUSH", "1")], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_to_string(&log).unwrap(), "--reason sigterm\n");
}

#[test]
fn the_idle_exit_status_is_the_one_the_app_reads() {
    assert!(
        block(ENTRYPOINT, "aws_stop")
            .contains(&format!("triplec_idle_exit={}\n", crate::aws::tasks::IDLE_EXIT_CODE)),
        "entrypoint.sh and aws::tasks::IDLE_EXIT_CODE disagree"
    );
    assert!(
        block(ENTRYPOINT, "aws_stop")
            .contains(&format!("triplec_lifetime_exit={}\n", crate::aws::tasks::LIFETIME_EXIT_CODE)),
        "entrypoint.sh and aws::tasks::LIFETIME_EXIT_CODE disagree"
    );
    assert!(ENTRYPOINT.contains("exit \"${triplec_exit_status:-0}\""), "stop_keepalive exits with it");
    // The watcher gets the WIP setting through its scrubbed environment.
    assert!(block(ENTRYPOINT, "aws_stop").contains("TRIPLEC_WIP_PUSH=\"${TRIPLEC_WIP_PUSH:-0}\""));
    assert!(block(ENTRYPOINT, "aws_stop")
        .contains("TRIPLEC_MAX_LIFETIME_MINUTES=\"${TRIPLEC_MAX_LIFETIME_MINUTES:-}\""));
    assert!(IDLE_WATCH.contains("printf 'lifetime\\n' > \"$reason_file\""));
}

#[test]
fn start_clone_repos_marks_done_however_the_clone_ends() {
    let tmp = tempfile::tempdir().unwrap();
    let status = tmp.path().join("status");
    // clone_repos replaced by one that fails half-way.
    let script = format!(
        "{}\nclone_repos() {{ exit 3; }}\nstart_clone_repos '[x]' \"$1\"\nwait\n",
        aws_run_blocks(tmp.path(), &["aws_env", "aws_run", "aws_repos"])
    );
    let out = bash(&script, &[&status], &[], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(status.join("done").exists(), "a failed clone still ends: {}", text(&out));
}

#[test]
fn docker_mode_never_reaches_the_aws_blocks() {
    // Every new step sits behind TRIPLEC_HOST=aws; the scripts are only
    // started from those branches.
    for name in ["aws_env", "aws_run", "aws_home", "aws_repos", "aws_stop"] {
        let b = eblock(name);
        assert!(b.contains("aws"), "{name}");
    }
    let outside = |text: &str, names: &[&str]| {
        let mut s = text.to_string();
        for name in names {
            s = s.replace(block(text, name), "");
        }
        s
    };
    let outside_ep = outside(ENTRYPOINT, &["aws_env", "aws_run", "aws_stop"]);
    // The library is sourced by Docker containers too: outside its aws
    // blocks, every aws step is guarded the same way.
    let outside_lib = outside(LIB, &["aws_lib", "aws_home", "aws_repos"]);
    let lines: Vec<&str> = outside_ep.lines().chain(outside_lib.lines()).collect();
    let calls = [
        "triple-c-wip-push",
        "triple-c-idle-watch",
        "clone_repos",
        "link_persistent_home",
        "aws_hybrid_ssh_dir",
        "aws_before_stop",
        "start_idle_watch",
        "aws_efs_workspace_check",
        "aws_prepare_run",
        "aws_run_setup",
        "triple-c-workspace-status",
        "triple-c-apply-connect",
        "aws_wait_connect",
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
    assert_eq!(
        seen, 7,
        "the call sites: run record + setup file, Hybrid/EFS link, EFS /workspace check, Hybrid ~/.ssh \
         (in merge_host_ssh), clone (+ setup), idle watch, stop"
    );
    // Clones start in the background *after* the agent: the app gives a
    // starting task's agent only seconds to answer.
    let pos = |needle: &str| {
        lines
            .iter()
            .position(|l| l.trim_start().starts_with(needle))
            .unwrap_or_else(|| panic!("{needle}"))
    };
    let agent_call = lines.iter().position(|l| l.trim() == "start_agent").expect("start_agent call");
    assert!(agent_call < pos("start_clone_repos"), "clones must start after the agent");
    // The stop trap is in place before any AWS background work starts, so a
    // TERM in between still runs aws_before_stop.
    let trap = pos("trap stop_keepalive TERM INT HUP");
    assert!(trap < pos("start_clone_repos"), "the trap must precede the clone");
    assert!(trap < pos("start_idle_watch"), "the trap must precede the idle watcher");
    assert!(pos("stop_keepalive()") < trap);
    assert!(eblock("aws_repos").contains(") &"), "start_clone_repos backgrounds the clone");
    // And the snapshot-sensitive variables are gone before the agent's
    // environment snapshot is taken.
    assert!(ENTRYPOINT.find("unset TRIPLEC_FILES").unwrap() < ENTRYPOINT.find("# >>> agent_token").unwrap());
    assert!(ENTRYPOINT.find("unset TRIPLEC_REPOS").unwrap() < ENTRYPOINT.find("# >>> agent_token").unwrap());
    assert!(
        ENTRYPOINT.find("unset TRIPLEC_SETUP_SCRIPT").unwrap() < ENTRYPOINT.find("# >>> agent_token").unwrap()
    );
    // The run record is made after the agent-token block (its children must
    // not inherit TRIPLEC_AGENT_TOKEN) and before anything that can push.
    let prepare = ENTRYPOINT.find("then aws_prepare_run; fi").expect("aws_prepare_run call");
    assert!(prepare > ENTRYPOINT.find("# <<< agent_token").unwrap());
    assert!(prepare < ENTRYPOINT.find("trap stop_keepalive TERM INT HUP").unwrap());
    assert!(prepare < ENTRYPOINT.find("\nstart_agent\n").unwrap());
    // The setup runs in the clone's background chain, never in the agent's way.
    assert!(eblock("aws_repos").contains("aws_run_setup"));
}

// ------------------------------------------- run record and setup script

/// The named blocks with the run and setup dirs moved into `tmp` (`run`,
/// `setup`) and the container user switched off.
fn aws_run_blocks(tmp: &Path, names: &[&str]) -> String {
    aws_blocks(names)
        .replace(
            "triplec_run_dir=/run/triple-c\n",
            &format!("triplec_run_dir='{}'\n", tmp.join("run").display()),
        )
        .replace(
            "triplec_setup_dir=/run/triple-c-setup\n",
            &format!("triplec_setup_dir='{}'\n", tmp.join("setup").display()),
        )
}

/// A one-shot fake ECS task metadata endpoint answering `/task` with `body`.
fn fake_metadata(body: &str) -> (String, std::process::Child) {
    let server = r#"
import http.server, sys, socketserver
body = sys.argv[1].encode()
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200 if self.path == "/v4/x/task" else 404)
        self.end_headers()
        if self.path == "/v4/x/task": self.wfile.write(body)
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
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    use std::io::BufRead;
    std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    (format!("http://127.0.0.1:{}/v4/x", line.trim()), child)
}

fn is_run_id(s: &str, suffix: &str) -> bool {
    let b = s.as_bytes();
    s.len() == 16 + suffix.len()
        && b[8] == b'-'
        && b[15] == b'-'
        && s[..8].chars().chain(s[9..15].chars()).all(|c| c.is_ascii_digit())
        && &s[16..] == suffix
}

#[test]
fn aws_prepare_run_records_the_run_and_writes_the_setup_script() {
    let tmp = tempfile::tempdir().unwrap();
    let script = format!(
        "{}\ntriplec_setup_script=\"$SETUP\"\naws_prepare_run\n",
        aws_run_blocks(tmp.path(), &["aws_env", "aws_run"])
    );
    let before = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let out = bash(&script, &[], &[("SETUP", "echo hi\n")], &["ECS_CONTAINER_METADATA_URI_V4", "TRIPLEC_HOST"]);
    assert!(out.status.success(), "{}", text(&out));
    let run = tmp.path().join("run");
    let setup = tmp.path().join("setup");
    let id = fs::read_to_string(run.join("run-id")).unwrap();
    assert!(is_run_id(id.trim_end(), "local"), "{id:?}");
    let epoch: u64 = fs::read_to_string(run.join("start-epoch")).unwrap().trim().parse().unwrap();
    assert!(epoch >= before && epoch <= before + 5);
    assert_eq!(mode(&run), 0o711, "claude may read the run id, not list the dir");
    assert_eq!(mode(&run.join("run-id")), 0o644);
    assert_eq!(mode(&run.join("wip-last")), 0o700);
    assert_eq!(mode(&setup), 0o755);
    assert_eq!(fs::read_to_string(setup.join("script")).unwrap(), "echo hi\n");
    assert_eq!(mode(&setup.join("script")), 0o644);
    assert_eq!(fs::read_to_string(setup.join("status")).unwrap(), "running\n");
    assert_eq!(fs::read_to_string(setup.join("log")).unwrap(), "");

    // No script: nothing for the setup to run, no status (state `none`).
    let out = bash(&script, &[], &[("SETUP", "")], &["ECS_CONTAINER_METADATA_URI_V4"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(setup.is_dir() && !setup.join("script").exists() && !setup.join("status").exists());

    // On ECS the run id carries the task id's first 8 characters.
    let (url, mut meta) = fake_metadata(
        r#"{"TaskARN":"arn:aws:ecs:eu-west-1:123456789012:task/triple-c/0123abcd4567ef890123abcd4567ef89"}"#,
    );
    let out = bash(&script, &[], &[("SETUP", ""), ("ECS_CONTAINER_METADATA_URI_V4", &url)], &[]);
    meta.kill().ok();
    assert!(out.status.success(), "{}", text(&out));
    let id = fs::read_to_string(run.join("run-id")).unwrap();
    assert!(is_run_id(id.trim_end(), "0123abcd"), "{id:?}");
    // An answer that is not a task id, or none at all, falls back to `local`.
    let (bad_url, mut bad_meta) = fake_metadata(r#"{"TaskARN":"arn:aws:ecs:x:1:task/c/a-b"}"#);
    for uri in [bad_url.as_str(), "http://127.0.0.1:9/v4"] {
        let out = bash(&script, &[], &[("SETUP", ""), ("ECS_CONTAINER_METADATA_URI_V4", uri)], &[]);
        assert!(out.status.success(), "{}", text(&out));
        let id = fs::read_to_string(run.join("run-id")).unwrap();
        assert!(is_run_id(id.trim_end(), "local"), "{uri}: {id:?}");
    }
    bad_meta.kill().ok();
}

/// Runs `start_clone_repos` with `setup_script` (and a clone that takes a
/// moment), as the entrypoint would; `_triplec_as_user` records its argv.
fn run_setup(tmp: &Path, setup_script: &str, timeout: &str) -> Output {
    let blocks = aws_run_blocks(tmp, &["aws_env", "aws_run", "aws_repos"])
        .replace("triplec_setup_timeout=1800\n", &format!("triplec_setup_timeout={timeout}\n"));
    let script = format!(
        "{blocks}\n\
         _triplec_as_user() {{ printf '%s\\n' \"$*\" >> \"$CALLS\"; \"$@\"; }}\n\
         clone_repos() {{ sleep 1; echo cloned > \"$3/marker\"; }}\n\
         triplec_setup_script=\"$SETUP\"\n\
         aws_prepare_run\n\
         start_clone_repos '[]' \"$STATUS\"\n\
         wait\n"
    );
    bash(
        &script,
        &[],
        &[
            ("SETUP", setup_script),
            ("CALLS", tmp.join("calls").to_str().unwrap()),
            ("STATUS", tmp.join("clone").to_str().unwrap()),
        ],
        &["ECS_CONTAINER_METADATA_URI_V4"],
    )
}

#[test]
fn the_setup_script_runs_after_the_clone_as_the_user_with_no_stdin() {
    let tmp = tempfile::tempdir().unwrap();
    let clone = tmp.path().join("clone");
    let body = format!(
        "echo started\nreadlink /proc/self/fd/0\n\
         if [ -e '{c}/done' ] && [ -e '{c}/marker' ]; then echo clone-was-done; fi\n",
        c = clone.display()
    );
    let out = run_setup(tmp.path(), &body, "1800");
    assert!(out.status.success(), "{}", text(&out));
    let setup = tmp.path().join("setup");
    let log = fs::read_to_string(setup.join("log")).unwrap();
    assert_eq!(log, "started\n/dev/null\nclone-was-done\n");
    assert_eq!(fs::read_to_string(setup.join("status")).unwrap(), "ok\n");
    let calls = fs::read_to_string(tmp.path().join("calls")).unwrap();
    assert_eq!(
        calls,
        format!("timeout -k 10 1800 bash -e {}/script\n", setup.display()),
        "as the user, bounded, under bash -e"
    );
    assert!(text(&out).contains("entrypoint: setup script ok"), "{}", text(&out));
}

#[test]
fn the_setup_script_fails_under_bash_e_and_a_hang_is_killed_as_failed_124() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_setup(tmp.path(), "echo one\nfalse\necho never\n", "1800");
    assert!(out.status.success(), "a failed setup never fails the start: {}", text(&out));
    let setup = tmp.path().join("setup");
    assert_eq!(fs::read_to_string(setup.join("status")).unwrap(), "failed 1\n");
    assert_eq!(fs::read_to_string(setup.join("log")).unwrap(), "one\n");

    let tmp = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    let out = run_setup(tmp.path(), "echo waiting\nsleep 60\n", "1");
    assert!(out.status.success(), "{}", text(&out));
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "the hang was killed");
    let setup = tmp.path().join("setup");
    assert_eq!(fs::read_to_string(setup.join("status")).unwrap(), "failed 124\n");
}

#[test]
fn no_setup_script_means_no_setup_run() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_setup(tmp.path(), "", "1800");
    assert!(out.status.success(), "{}", text(&out));
    assert!(!tmp.path().join("calls").exists(), "nothing ran as the user");
    assert!(!tmp.path().join("setup/status").exists());
    // The clone status is still made, and ends `done`, with no repos at all.
    assert!(tmp.path().join("clone/done").exists());
}

#[test]
fn idle_watch_check_counts_a_running_setup_as_busy() {
    let w = Watch::new();
    let (url, mut agent) = fake_agent(w.tmp.path(), OLD_ACTIVITY);
    let setup = w.tmp.path().join("setup");
    fs::create_dir_all(&setup).unwrap();
    fs::write(setup.join("status"), "running\n").unwrap();
    let check = || w.cmd(&url, 0).env("TRIPLEC_SETUP_STATUS_DIR", &setup).arg("--check").output().unwrap();
    let busy = check();
    fs::write(setup.join("status"), "failed 124\n").unwrap();
    let failed = check();
    fs::write(setup.join("status"), "ok\n").unwrap();
    let ok = check();
    agent.kill().ok();
    assert_eq!(busy.status.code(), Some(1), "{}", text(&busy));
    assert_eq!(failed.status.code(), Some(0), "{}", text(&failed));
    assert_eq!(ok.status.code(), Some(0), "{}", text(&ok));
}

// ----------------------------------------------------------- lifetime cap

impl Watch {
    /// A watcher for the lifetime cap alone (no idle stop), its run started
    /// `age_secs` ago.
    fn lifetime_cmd(&self, signal_pid: u32, minutes: &str, age_secs: u64) -> Command {
        let p = self.tmp.path();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        fs::write(p.join("start-epoch"), format!("{}\n", now - age_secs)).unwrap();
        // An agent that never answers: the idle check alone would never stop it.
        let mut c = self.cmd("http://127.0.0.1:9", signal_pid);
        c.env_remove("TRIPLEC_IDLE_STOP_MINUTES")
            .env("TRIPLEC_MAX_LIFETIME_MINUTES", minutes)
            .env("TRIPLEC_START_EPOCH_FILE", p.join("start-epoch"));
        c
    }
}

#[test]
fn lifetime_cap_pushes_then_stops_with_its_marker_even_when_the_push_fails() {
    let w = Watch::new();
    w.dirty();
    fs::write(w.tmp.path().join("push_rc"), "1").unwrap();
    let mut target = fake_entrypoint(w.tmp.path());
    let mut cmd = w.lifetime_cmd(target.id(), "1", 61);
    cmd.env("TRIPLEC_WIP_PUSH", "1");
    let out = run_watch_for(cmd, 20).expect("it stops at the cap");
    target.wait().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(w.pushes(), "--reason lifetime\n", "one push, best effort");
    assert!(text(&out).contains("stopping anyway"), "{}", text(&out));
    assert_eq!(fs::read_to_string(w.tmp.path().join("got")).unwrap().trim(), "term");
    assert_eq!(fs::read_to_string(w.tmp.path().join("reason")).unwrap().trim(), "lifetime");
}

#[test]
fn lifetime_cap_with_the_wip_push_off_stops_without_pushing() {
    let w = Watch::new();
    w.dirty();
    let mut target = fake_entrypoint(w.tmp.path());
    let mut cmd = w.lifetime_cmd(target.id(), "1", 120);
    cmd.env("TRIPLEC_WIP_PUSH", "0");
    let out = run_watch_for(cmd, 20).expect("it stops at the cap");
    target.wait().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(w.pushes(), "");
    assert_eq!(fs::read_to_string(w.tmp.path().join("reason")).unwrap().trim(), "lifetime");
}

#[test]
fn lifetime_cap_not_reached_keeps_the_workspace_up() {
    let w = Watch::new();
    let mut target = fake_entrypoint(w.tmp.path());
    let ran = run_watch_for(w.lifetime_cmd(target.id(), "60", 30), 3);
    target.kill().ok();
    target.wait().ok();
    assert!(ran.is_none(), "{:?}", ran.map(|o| text(&o)));
    assert!(!w.tmp.path().join("got").exists());
    // Neither an idle stop nor a cap: nothing to watch, refused.
    let none = w.cmd("http://127.0.0.1:9", target.id()).env_remove("TRIPLEC_IDLE_STOP_MINUTES").output().unwrap();
    assert_eq!(none.status.code(), Some(2), "{}", text(&none));
    let zero = w.lifetime_cmd(target.id(), "0", 30).output().unwrap();
    assert_eq!(zero.status.code(), Some(2), "0 is no cap: {}", text(&zero));
    let zeros = w.lifetime_cmd(target.id(), "00", 30).output().unwrap();
    assert_eq!(zeros.status.code(), Some(2), "00 is no cap either: {}", text(&zeros));
}

#[test]
fn lifetime_cap_leaves_a_stop_guard_decision_alone() {
    for marker in ["pushed", "discard"] {
        let w = Watch::new();
        w.dirty();
        fs::write(w.tmp.path().join("reason"), format!("{marker}\n")).unwrap();
        let mut target = fake_entrypoint(w.tmp.path());
        let mut cmd = w.lifetime_cmd(target.id(), "1", 120);
        cmd.env("TRIPLEC_WIP_PUSH", "1");
        let out = run_watch_for(cmd, 20).expect("it signals");
        target.wait().unwrap();
        assert!(out.status.success(), "{}", text(&out));
        assert_eq!(w.pushes(), "", "{marker}: the app already decided");
        assert_eq!(fs::read_to_string(w.tmp.path().join("reason")).unwrap().trim(), marker);
    }
}

// ------------------------------------------------------- WIP branch per run

fn wip_push_in_run(ws: &Path, run_id: Option<&str>) -> Output {
    let tmp = ws.parent().unwrap();
    let run = tmp.join("run");
    fs::create_dir_all(run.join("wip-last")).unwrap();
    match run_id {
        Some(id) => fs::write(run.join("run-id"), format!("{id}\n")).unwrap(),
        None => {
            let _ = fs::remove_file(run.join("run-id"));
        }
    }
    let script = write_script(tmp, "triple-c-wip-push", WIP_PUSH);
    Command::new("bash")
        .arg(script)
        .args(["--reason", "lifetime"])
        .env("TRIPLEC_WORKSPACE_ROOT", ws)
        .env("TRIPLEC_CLONE_STATUS_DIR", tmp.join("no-clone-status"))
        .env("TRIPLEC_RUN_ID_FILE", run.join("run-id"))
        .env("TRIPLEC_WIP_LAST_DIR", run.join("wip-last"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", tmp)
        .output()
        .unwrap()
}

#[test]
fn wip_pushes_in_one_run_update_one_branch_and_skip_an_unchanged_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, repo, remote) = workspace_with_clone(tmp.path());
    let run_id = "20261115-093000-0123abcd";
    fs::write(repo.join("a.txt"), "one").unwrap();
    let out = wip_push_in_run(&ws, Some(run_id));
    assert!(out.status.success(), "{}", text(&out));
    let branch = format!("refs/heads/triple-c/wip/{run_id}-proj");
    assert_eq!(wip_refs(&remote), [branch.clone()]);
    let first = git(&remote, &["rev-parse", &branch]);
    assert_eq!(git(&remote, &["show", &format!("{branch}:a.txt")]), "one");
    let last = tmp.path().join("run/wip-last/proj");
    assert_eq!(
        fs::read_to_string(&last).unwrap(),
        format!("{} origin\n", git(&remote, &["rev-parse", &format!("{branch}^{{tree}}")]))
    );

    // Nothing changed: skipped, the branch stays where it was.
    let out = wip_push_in_run(&ws, Some(run_id));
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("unchanged since the last push"), "{}", text(&out));
    assert_eq!(git(&remote, &["rev-parse", &branch]), first);

    // A change: the same branch moves (forced — the WIP commit is not a
    // descendant of the last one), still one branch for the run.
    fs::write(repo.join("a.txt"), "two").unwrap();
    let out = wip_push_in_run(&ws, Some(run_id));
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(wip_refs(&remote), [branch.clone()]);
    assert_ne!(git(&remote, &["rev-parse", &branch]), first);
    assert_eq!(git(&remote, &["show", &format!("{branch}:a.txt")]), "two");
    assert_eq!(git(&remote, &["log", "-1", "--format=%s", &branch]).contains("(lifetime)"), true);

    // A failed push records nothing, so the next one tries again.
    fs::write(repo.join("a.txt"), "three").unwrap();
    let before = fs::read_to_string(&last).unwrap();
    let saved = tmp.path().join("origin.git.away");
    fs::rename(&remote, &saved).unwrap();
    let out = wip_push_in_run(&ws, Some(run_id));
    assert!(!out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_to_string(&last).unwrap(), before);
    fs::rename(&saved, &remote).unwrap();
    let out = wip_push_in_run(&ws, Some(run_id));
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(git(&remote, &["show", &format!("{branch}:a.txt")]), "three");
}

#[test]
fn wip_push_without_a_usable_run_id_keeps_the_per_push_stamp() {
    for run_id in [None, Some("../../evil"), Some("not a run id")] {
        let tmp = tempfile::tempdir().unwrap();
        let (ws, repo, remote) = workspace_with_clone(tmp.path());
        fs::write(repo.join("a.txt"), "one").unwrap();
        let out = wip_push_in_run(&ws, run_id);
        assert!(out.status.success(), "{}", text(&out));
        let refs = wip_refs(&remote);
        assert_eq!(refs.len(), 1, "{refs:?}");
        let name = refs[0].trim_start_matches("refs/heads/triple-c/wip/");
        assert!(name.len() == 20 && name.as_bytes()[8] == b'-' && name.ends_with("-proj"), "{run_id:?}: {name}");
    }
}

// ------------------------------------------------------- workspace status

fn workspace_status(clone: &Path, setup: &Path) -> serde_json::Value {
    let tmp = clone.parent().unwrap();
    let script = write_script(tmp, "triple-c-workspace-status", WORKSPACE_STATUS);
    let out = Command::new("python3")
        .arg("-I")
        .arg(script)
        .env("TRIPLEC_CLONE_STATUS_DIR", clone)
        .env("TRIPLEC_SETUP_STATUS_DIR", setup)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1, "one JSON object on one line: {stdout}");
    serde_json::from_str(&stdout).unwrap()
}

#[test]
fn workspace_status_reports_clone_and_setup_and_survives_missing_files() {
    let tmp = tempfile::tempdir().unwrap();
    let clone = tmp.path().join("clone");
    let setup = tmp.path().join("setup");
    // Nothing yet: the clone has not started, no setup.
    assert_eq!(
        workspace_status(&clone, &setup),
        serde_json::json!({"clone": {"done": false, "repos": []},
                           "setup": {"state": "none", "code": null, "tail": []}})
    );

    fs::create_dir_all(&clone).unwrap();
    fs::write(clone.join("repos"), "api\tcloning\napi\tcloned\nweb\tcloning\n").unwrap();
    fs::create_dir_all(&setup).unwrap();
    fs::write(setup.join("status"), "running\n").unwrap();
    let v = workspace_status(&clone, &setup);
    assert_eq!(
        v["clone"],
        serde_json::json!({"done": false, "repos": [
            {"folder": "api", "state": "cloned"}, {"folder": "web", "state": "cloning"}]})
    );
    assert_eq!(v["setup"], serde_json::json!({"state": "running", "code": null, "tail": []}));

    fs::write(clone.join("repos"), "api\tcloning\napi\tcloned\nweb\tcloning\nweb\tfailed\n").unwrap();
    fs::write(clone.join("done"), "").unwrap();
    let lines: String = (1..=25).map(|i| format!("line {i}\n")).collect();
    fs::write(setup.join("log"), format!("{lines}partial \u{e9}\u{ff}")).unwrap();
    fs::write(setup.join("status"), "ok\n").unwrap();
    let v = workspace_status(&clone, &setup);
    assert_eq!(v["clone"]["done"], true);
    assert_eq!(v["clone"]["repos"][1], serde_json::json!({"folder": "web", "state": "failed"}));
    assert_eq!(v["setup"]["state"], "ok");
    assert_eq!(v["setup"]["code"], 0);
    let tail: Vec<String> =
        v["setup"]["tail"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_string()).collect();
    assert_eq!(tail.len(), 20);
    assert_eq!(tail[0], "line 7");
    assert!(tail[19].starts_with("partial \u{e9}"), "{:?}", tail[19]);

    fs::write(setup.join("status"), "failed 124\n").unwrap();
    let v = workspace_status(&clone, &setup);
    assert_eq!((v["setup"]["state"].as_str(), v["setup"]["code"].as_i64()), (Some("failed"), Some(124)));
    // Garbled status: a failure with no code, never a crash.
    fs::write(setup.join("status"), "what\n").unwrap();
    let v = workspace_status(&clone, &setup);
    assert_eq!(v["setup"], serde_json::json!({"state": "failed", "code": null, "tail": v["setup"]["tail"]}));
    // An unreadable log is an empty tail.
    fs::remove_file(setup.join("log")).unwrap();
    fs::create_dir(setup.join("log")).unwrap();
    assert_eq!(workspace_status(&clone, &setup)["setup"]["tail"], serde_json::json!([]));
}

#[test]
fn lifetime_cap_reads_leading_zeros_as_decimal() {
    // `08` is 8 minutes, not a bad octal number.
    let w = Watch::new();
    let mut target = fake_entrypoint(w.tmp.path());
    let out = run_watch_for(w.lifetime_cmd(target.id(), "08", 8 * 60 + 1), 20).expect("it stops at 8 minutes");
    target.wait().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_to_string(w.tmp.path().join("reason")).unwrap().trim(), "lifetime");
    let w = Watch::new();
    let mut target = fake_entrypoint(w.tmp.path());
    let ran = run_watch_for(w.lifetime_cmd(target.id(), "08", 7 * 60), 3);
    target.kill().ok();
    target.wait().ok();
    assert!(ran.is_none(), "7 of 8 minutes: {:?}", ran.map(|o| text(&o)));
}

#[test]
fn a_setup_chain_that_dies_early_records_failed_so_the_idle_stop_is_not_blocked() {
    // The chain ends abnormally while the status still says `running`.
    let tmp = tempfile::tempdir().unwrap();
    let blocks = aws_run_blocks(tmp.path(), &["aws_env", "aws_run", "aws_repos"]);
    let script = format!(
        "{blocks}\n\
         aws_run_setup() {{ exit 9; }}\n\
         triplec_setup_script='echo hi'\n\
         aws_prepare_run\n\
         start_clone_repos '[]' \"$STATUS\"\n\
         wait\n"
    );
    let out = bash(&script, &[], &[("STATUS", tmp.path().join("clone").to_str().unwrap())], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_to_string(tmp.path().join("setup/status")).unwrap(), "failed\n");

    // Killed (TERM) while the clone is still running: `done` and `failed`.
    let tmp = tempfile::tempdir().unwrap();
    let blocks = aws_run_blocks(tmp.path(), &["aws_env", "aws_run", "aws_repos"]);
    let pid = tmp.path().join("inner.pid");
    let script = format!(
        "{blocks}\n\
         clone_repos() {{ sleep 30 >/dev/null 2>&1 & s=$!; echo \"$BASHPID $s\" > '{p}'; wait $s; }}\n\
         triplec_setup_script='echo hi'\n\
         aws_prepare_run\n\
         start_clone_repos '[]' \"$STATUS\"\n\
         chain=$!\n\
         while [ ! -s '{p}' ]; do sleep 0.1; done\n\
         kill -TERM \"$chain\" $(cat '{p}')\n\
         wait\n",
        p = pid.display()
    );
    let started = std::time::Instant::now();
    let out = bash(&script, &[], &[("STATUS", tmp.path().join("clone").to_str().unwrap())], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert!(tmp.path().join("clone/done").exists());
    assert_eq!(fs::read_to_string(tmp.path().join("setup/status")).unwrap(), "failed\n");
    // A finished setup's status is left alone by the trap.
    let tmp = tempfile::tempdir().unwrap();
    let out = run_setup(tmp.path(), "true\n", "1800");
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_to_string(tmp.path().join("setup/status")).unwrap(), "ok\n");
}

#[test]
fn each_folder_in_a_run_has_its_own_wip_branch_even_on_one_remote() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, api, remote) = workspace_with_clone(tmp.path());
    // A second clone of the same bare remote, under a folder name git refs
    // cannot carry as is.
    git(&ws, &["clone", "-q", remote.to_str().unwrap(), "we b@x"]);
    let web = ws.join("we b@x");
    git(&web, &["config", "user.name", "Dev"]);
    git(&web, &["config", "user.email", "dev@example.invalid"]);
    let run_id = "20261115-093000-0123abcd";
    fs::write(api.join("a.txt"), "api-1").unwrap();
    fs::write(web.join("a.txt"), "web-1").unwrap();
    let out = wip_push_in_run(&ws, Some(run_id));
    assert!(out.status.success(), "{}", text(&out));
    let api_ref = format!("refs/heads/triple-c/wip/{run_id}-proj");
    // Sanitised, so with the first 8 hex of sha256("we b@x").
    let web_ref = format!("refs/heads/triple-c/wip/{run_id}-we_b_x-7d2e374f");
    let mut refs = wip_refs(&remote);
    refs.sort();
    assert_eq!(refs, [api_ref.clone(), web_ref.clone()]);
    assert_eq!(git(&remote, &["show", &format!("{api_ref}:a.txt")]), "api-1");
    assert_eq!(git(&remote, &["show", &format!("{web_ref}:a.txt")]), "web-1");
    let web_first = git(&remote, &["rev-parse", &web_ref]);

    // A second push changes only api: its branch moves, web's is skipped.
    fs::write(api.join("a.txt"), "api-2").unwrap();
    let out = wip_push_in_run(&ws, Some(run_id));
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("we b@x: unchanged since the last push"), "{}", text(&out));
    assert_eq!(git(&remote, &["show", &format!("{api_ref}:a.txt")]), "api-2");
    assert_eq!(git(&remote, &["rev-parse", &web_ref]), web_first);
    assert_eq!(wip_refs(&remote).len(), 2);
}

#[test]
fn a_folder_name_no_ref_can_carry_falls_back_to_a_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _, remote) = workspace_with_clone(tmp.path());
    git(&ws, &["clone", "-q", remote.to_str().unwrap(), "a..b.lock"]);
    let repo = ws.join("a..b.lock");
    git(&repo, &["config", "user.name", "Dev"]);
    git(&repo, &["config", "user.email", "dev@example.invalid"]);
    fs::write(repo.join("x"), "x").unwrap();
    let run_id = "20261115-093000-local";
    let out = wip_push_in_run(&ws, Some(run_id));
    assert!(out.status.success(), "{}", text(&out));
    let refs = wip_refs(&remote);
    assert_eq!(refs.len(), 1, "{refs:?}");
    let suffix = refs[0].strip_prefix(&format!("refs/heads/triple-c/wip/{run_id}-")).unwrap();
    assert!(suffix.len() == 12 && suffix.chars().all(|c| c.is_ascii_hexdigit()), "{suffix}");
}

#[test]
fn folders_that_sanitise_to_the_same_name_get_different_wip_branches() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _, remote) = workspace_with_clone(tmp.path());
    for folder in ["a b", "a_b"] {
        git(&ws, &["clone", "-q", remote.to_str().unwrap(), folder]);
        let repo = ws.join(folder);
        git(&repo, &["config", "user.name", "Dev"]);
        git(&repo, &["config", "user.email", "dev@example.invalid"]);
        fs::write(repo.join("x"), folder).unwrap();
    }
    let run_id = "20261115-093000-local";
    let out = wip_push_in_run(&ws, Some(run_id));
    assert!(out.status.success(), "{}", text(&out));
    let mut refs = wip_refs(&remote);
    refs.sort();
    let base = format!("refs/heads/triple-c/wip/{run_id}-");
    // Exact names keep their folder; the changed one gets a hash of the raw name.
    assert!(refs.iter().any(|r| *r == format!("{base}a_b")), "{refs:?}");
    let spaced: Vec<_> = refs.iter().filter_map(|r| r.strip_prefix(&format!("{base}a_b-"))).collect();
    assert_eq!(spaced.len(), 1, "{refs:?}");
    assert!(spaced[0].len() == 8 && spaced[0].chars().all(|c| c.is_ascii_hexdigit()), "{refs:?}");
    assert_eq!(refs.len(), 2, "{refs:?}");
}

// ------------------------------------------------- copy-at-connect (phase 6 Q2)

#[test]
fn sourcing_the_library_only_defines_functions() {
    // Docker parity: the entrypoint sources it first thing, so it must run
    // nothing — no output, no files, no environment changes.
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("lib.sh");
    fs::write(&lib, LIB).unwrap();
    let out = Command::new("env")
        .args(["-i", "PATH=/usr/bin:/bin", "bash", "-c"])
        .arg(". \"$1\" && env && echo --- && declare -F | awk '{print $3}'")
        .arg("t")
        .arg(&lib)
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(out.stderr.is_empty(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (env, funcs) = stdout.split_once("---\n").unwrap();
    assert!(!env.contains("triplec_") && !env.contains("CA_"), "nothing exported: {env}");
    for f in [
        "_triplec_as_user",
        "link_persistent_home",
        "aws_hybrid_ssh_dir",
        "install_corporate_ca",
        "sync_host_ssh",
        "merge_host_ssh",
        "install_git_credentials",
        "clone_repos",
        "aws_wait_connect",
        "start_clone_repos",
    ] {
        assert!(funcs.lines().any(|l| l == f), "{f} missing: {funcs}");
    }
    assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1, "sourcing made no files");
}

#[test]
fn the_entrypoint_sources_the_library_first_and_calls_the_moved_steps_in_order() {
    let path = "/usr/local/lib/triple-c/entrypoint-lib.sh";
    let source = ENTRYPOINT.find(&format!(". {path}")).expect("sources the library");
    assert!(source < ENTRYPOINT.find("# >>> aws_env").unwrap(), "before any block uses it");
    let docker = include_str!("../../../../container/Dockerfile");
    assert!(docker.contains(&format!("COPY entrypoint-lib.sh {path}")));
    assert!(docker.contains("COPY triple-c-apply-connect /usr/local/bin/triple-c-apply-connect"));
    assert!(APPLY_CONNECT.contains(&format!("lib={path}\n")));
    // The steps run where the inline code ran: CA, then the SSH merge, then
    // the git credential helper, all before the clone starts.
    let at = |needle: &str| {
        ENTRYPOINT
            .lines()
            .position(|l| l.trim_start() == needle)
            .unwrap_or_else(|| panic!("{needle}"))
    };
    let ca = at("install_corporate_ca");
    let ssh = at("merge_host_ssh /tmp/.host-ssh");
    let git = at("install_git_credentials \"$GIT_TOKEN\"");
    assert!(at("link_persistent_home /home/claude \"$triplec_home_mnt\" claude:claude") < ca);
    assert!(ca < ssh && ssh < git && git < at("start_clone_repos \"$triplec_repos_json\" /run/triple-c-clone"));
    // Each block lives in exactly one file.
    for name in ["aws_lib", "aws_home", "ca_install", "sync_host_ssh", "merge_host_ssh", "git_credentials", "aws_repos"] {
        assert!(LIB.contains(&format!("# >>> {name}\n")), "{name}");
        assert!(!ENTRYPOINT.contains(&format!("# >>> {name}\n")), "{name}");
    }
}

#[test]
fn the_git_credential_helper_is_written_as_before() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let script = format!(
        "{}\ninstall_git_credentials \"$TOKEN\"\n",
        aws_blocks(&["git_credentials"])
            .replace("/home/claude", home.to_str().unwrap())
            .replace("chown claude:claude", ": chown")
    );
    let out = bash(&script, &[], &[("TOKEN", "tok-123")], &[]);
    assert!(out.status.success(), "{}", text(&out));
    let cred = home.join(".git-credentials");
    assert_eq!(
        fs::read_to_string(&cred).unwrap(),
        "https://oauth2:tok-123@github.com\nhttps://oauth2:tok-123@gitlab.com\nhttps://oauth2:tok-123@bitbucket.org\n"
    );
    assert_eq!(mode(&cred), 0o600);
    let helper = Command::new("git")
        .args(["config", "--file"])
        .arg(home.join(".gitconfig"))
        .args(["--get", "credential.helper"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&helper.stdout).trim(),
        format!("store --file={}", cred.display())
    );
}

#[test]
fn merge_host_ssh_merges_and_fixes_modes_and_refuses_a_foreign_link_on_aws() {
    let tmp = tempfile::tempdir().unwrap();
    let (home, mnt) = hybrid_dirs(tmp.path());
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("id_ed25519"), "KEY").unwrap();
    fs::write(src.join("id_ed25519.pub"), "PUB").unwrap();
    fs::write(src.join("config"), "Host *\n").unwrap();
    let run = |aws: &str| {
        let script = format!(
            "{}\ntriplec_aws={aws}\nmerge_host_ssh \"$1\"\n",
            aws_blocks(&["aws_home", "sync_host_ssh", "merge_host_ssh"])
                .replace("/home/claude", home.to_str().unwrap())
                .replace("triplec_home_mnt=/mnt/triple-c-home", &format!("triplec_home_mnt='{}'", mnt.display()))
                .replace("chown -R claude:claude", ": chown")
        );
        bash(&script, &[&src], &[], &[])
    };
    // Docker (and AWS without a link): ~/.ssh itself.
    let out = run("0");
    assert!(out.status.success(), "{}", text(&out));
    let ssh = home.join(".ssh");
    assert_eq!(fs::read_to_string(ssh.join("id_ed25519")).unwrap(), "KEY");
    assert_eq!(mode(&ssh), 0o700);
    assert_eq!(mode(&ssh.join("id_ed25519")), 0o600);
    assert_eq!(mode(&ssh.join("id_ed25519.pub")), 0o644);
    assert_eq!(mode(&ssh.join("config")), 0o600);

    // AWS Hybrid: through the link Triple-C made, into the mount.
    fs::remove_dir_all(&ssh).unwrap();
    fs::create_dir_all(mnt.join(".ssh")).unwrap();
    std::os::unix::fs::symlink(mnt.join(".ssh"), &ssh).unwrap();
    let out = run("1");
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_to_string(mnt.join(".ssh/id_ed25519")).unwrap(), "KEY");

    // Any other link: nothing merged.
    let elsewhere = tmp.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::remove_file(&ssh).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &ssh).unwrap();
    let out = run("1");
    assert!(text(&out).contains("symlink Triple-C did not make"), "{}", text(&out));
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
}

/// `start_clone_repos` under copy-at-connect, with `clone_repos` stubbed to
/// record when it ran.
fn connect_clone(tmp: &Path, marker: bool) -> (Output, std::time::Duration) {
    let run = tmp.join("run");
    fs::create_dir_all(&run).unwrap();
    if marker {
        fs::write(run.join("connected"), "").unwrap();
    }
    let script = format!(
        "{}\ntriplec_connect=1\n\
         clone_repos() {{ echo cloned > \"$3/marker\"; }}\n\
         aws_run_setup() {{ :; }}\n\
         start_clone_repos '[{{\"url\":\"u\",\"branch\":null,\"folder\":\"f\"}}]' \"$1\"\nwait\n",
        aws_run_blocks(tmp, &["aws_env", "aws_run", "aws_repos"])
            .replace("triplec_connect_wait=300\n", "triplec_connect_wait=2\n")
    );
    let started = std::time::Instant::now();
    let out = bash(&script, &[&tmp.join("status")], &[], &[]);
    (out, started.elapsed())
}

#[test]
fn under_copy_at_connect_the_clone_waits_for_the_connect_then_goes_ahead_without() {
    // Connected already: clones at once, with nothing to retry later.
    let tmp = tempfile::tempdir().unwrap();
    let (out, took) = connect_clone(tmp.path(), true);
    assert!(out.status.success(), "{}", text(&out));
    let status = tmp.path().join("status");
    assert!(status.join("marker").exists() && status.join("done").exists());
    assert!(!status.join("no-credentials").exists());
    assert!(took < std::time::Duration::from_secs(2), "{took:?}");

    // Never connected: waits out the limit, clones anyway, says so.
    let tmp = tempfile::tempdir().unwrap();
    let (out, took) = connect_clone(tmp.path(), false);
    assert!(out.status.success(), "{}", text(&out));
    let status = tmp.path().join("status");
    assert!(status.join("marker").exists() && status.join("done").exists());
    assert!(status.join("no-credentials").exists());
    assert!(took >= std::time::Duration::from_secs(2), "{took:?}");
    assert!(text(&out).contains("going ahead without the project's credentials"), "{}", text(&out));
}

/// Copy-at-connect with no repositories: `start_clone_repos` with a setup
/// script (or none), the connect arriving after a second; the stubbed setup
/// records whether the connect had happened when it ran.
fn connect_setup(tmp: &Path, setup_script: &str) -> (Output, std::time::Duration) {
    let script = format!(
        "{}
triplec_connect=1
         triplec_setup_script=\"$SETUP\"
         aws_prepare_run
         aws_run_setup() {{ if [ -e \"$triplec_run_dir/connected\" ]; then echo after > \"$OUT/setup-ran\"; else echo before > \"$OUT/setup-ran\"; fi; }}
         ( sleep 1; : > \"$triplec_run_dir/connected\" ) &
         start_clone_repos '[]' \"$1/status\"
wait
",
        aws_run_blocks(tmp, &["aws_env", "aws_run", "aws_repos"])
            .replace("triplec_connect_wait=300\n", "triplec_connect_wait=10\n")
    );
    let started = std::time::Instant::now();
    let out = bash(
        &script,
        &[tmp],
        &[("SETUP", setup_script), ("OUT", tmp.to_str().unwrap())],
        &["ECS_CONTAINER_METADATA_URI_V4"],
    );
    (out, started.elapsed())
}

#[test]
fn under_copy_at_connect_a_setup_script_waits_for_the_connect_even_with_no_repos() {
    let tmp = tempfile::tempdir().unwrap();
    let (out, took) = connect_setup(tmp.path(), "echo hi\n");
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_to_string(tmp.path().join("setup-ran")).unwrap(), "after\n", "{}", text(&out));
    assert!(took >= std::time::Duration::from_secs(1), "{took:?}");
    assert!(!tmp.path().join("status/no-credentials").exists());

    // No repositories and no setup script: nothing waits for the connect.
    let tmp = tempfile::tempdir().unwrap();
    let (out, took) = connect_setup(tmp.path(), "");
    assert!(out.status.success(), "{}", text(&out));
    assert!(tmp.path().join("status/done").exists());
    assert!(took < std::time::Duration::from_secs(3), "{took:?}");
}

/// A workspace for `triple-c-apply-connect`: its script and library with every
/// path moved into `tmp`, the user switched off, `update-ca-certificates`
/// faked and certutil skipped.
struct Connect {
    root: std::path::PathBuf,
    script: std::path::PathBuf,
    path_env: String,
}

impl Connect {
    fn new(tmp: &Path) -> Self {
        let p = |s: &str| tmp.join(s);
        for d in ["shm", "run", "home", "ws", "clone", "store", "bin", "lib"] {
            fs::create_dir_all(p(d)).unwrap();
        }
        let lib = LIB
            .replace("triplec_run_as=claude", "triplec_run_as=")
            .replace("triplec_run_dir=/run/triple-c\n", &format!("triplec_run_dir='{}'\n", p("run").display()))
            .replace("\"/usr/local/share/ca-certificates\"", &format!("'{}'", p("store").display()))
            .replace("\"/var/lib/triple-c/ca.stamp\"", &format!("'{}'", p("lib/ca.stamp").display()))
            .replace("\"/etc/ssl/certs/ca-certificates.crt\"", &format!("'{}'", p("lib/bundle").display()))
            .replace("if ! command -v certutil >/dev/null 2>&1; then", "if true; then")
            .replace("/home/claude", p("home").to_str().unwrap())
            .replace("chown -R claude:claude", ": chown")
            .replace("chown claude:claude", ": chown");
        fs::write(p("lib.sh"), lib).unwrap();
        fs::write(p("log"), "").unwrap();
        // The system bundle update-ca-certificates would have made.
        fs::write(p("lib/bundle"), PEM).unwrap();
        let script = APPLY_CONNECT
            .replace("lib=/usr/local/lib/triple-c/entrypoint-lib.sh\n", &format!("lib='{}'\n", p("lib.sh").display()))
            .replace("shm=/dev/shm/triple-c\n", &format!("shm='{}'\n", p("shm").display()))
            .replace("stage=/dev/shm/triple-c/connect\n", &format!("stage='{}'\n", p("shm/connect").display()))
            .replace("ca_dir=/dev/shm/triple-c/connect-ca\n", &format!("ca_dir='{}'\n", p("shm/connect-ca").display()))
            .replace("clone_status=/run/triple-c-clone\n", &format!("clone_status='{}'\n", p("clone").display()))
            .replace("workspace=/workspace\n", &format!("workspace='{}'\n", p("ws").display()))
            .replace("retry_wait=2400\n", "retry_wait=20\n")
            .replace("log_to=/proc/1/fd/1\n", &format!("log_to='{}'\n", p("log").display()))
            .replace("ssh_tmp_dir=/tmp\n", &format!("ssh_tmp_dir='{}'\n", tmp.display()));
        let script_path = write_script(tmp, "apply-connect", &script);
        write_script(&p("bin"), "update-ca-certificates", "#!/bin/sh\necho ran >> \"$(dirname \"$0\")/../update-calls\"\n");
        let path_env = format!("{}:{}", p("bin").display(), std::env::var("PATH").unwrap_or_default());
        Connect { root: tmp.to_path_buf(), script: script_path, path_env }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new("bash")
            .arg(&self.script)
            .args(args)
            .env("PATH", &self.path_env)
            .env_remove("GIT_TOKEN")
            .output()
            .unwrap()
    }

    fn p(&self, s: &str) -> std::path::PathBuf {
        self.root.join(s)
    }

    /// `--prepare`: this connect's nonce (its dir is `shm/connect/<nonce>`).
    fn prepare(&self) -> String {
        let out = self.run(&["--prepare"]);
        assert!(out.status.success(), "{}", text(&out));
        let nonce = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert!(nonce.len() == 16 && nonce.chars().all(|c| c.is_ascii_hexdigit()), "{nonce:?}");
        nonce
    }

    /// What the app uploads for one connect: `files` (`ssh/x`, `ca/y`) and
    /// `connect.env`.
    fn stage(&self, nonce: &str, files: &[(&str, &str)], env: &str) {
        for (k, v) in files {
            fs::write(self.p(&format!("shm/connect/{nonce}/{k}")), v).unwrap();
        }
        fs::write(self.p(&format!("shm/connect/{nonce}/connect.env")), env).unwrap();
    }

    /// A whole connect: prepare, stage, apply.
    fn connect(&self, files: &[(&str, &str)], env: &str) -> Output {
        let nonce = self.prepare();
        self.stage(&nonce, files, env);
        self.run(&[&nonce])
    }

    /// Waits for the background re-clone to finish (`done` back).
    fn wait_done(&self) {
        for _ in 0..200 {
            if self.p("clone/done").exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("the re-clone never finished: {}", fs::read_to_string(self.p("log")).unwrap());
    }
}

const PEM: &str = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";

#[test]
fn apply_connect_installs_the_ca_merges_ssh_writes_git_credentials_and_retries_only_failed_clones() {
    let tmp = tempfile::tempdir().unwrap();
    let c = Connect::new(tmp.path());
    let remote = bare_remote(tmp.path(), "remote");

    // A connect's own staging dir; one an abandoned connect left long ago is
    // cleared, a recent one (a connect in flight) is not.
    fs::create_dir_all(c.p("shm/connect/0000000000000old")).unwrap();
    fs::create_dir_all(c.p("shm/connect/00000000000recent")).unwrap();
    assert!(Command::new("touch").args(["-d", "1 hour ago"]).arg(c.p("shm/connect/0000000000000old")).status().unwrap().success());
    let nonce = c.prepare();
    for d in ["shm/connect".to_string(), format!("shm/connect/{nonce}"), format!("shm/connect/{nonce}/ssh"), format!("shm/connect/{nonce}/ca")] {
        assert_eq!(mode(&c.p(&d)), 0o700, "{d}");
    }
    assert!(!c.p("shm/connect/0000000000000old").exists());
    assert!(c.p("shm/connect/00000000000recent").exists());

    // What the app uploads.
    let token = "ghp_secret-value";
    let env = format!("ANTHROPIC_API_KEY={}\nGIT_TOKEN={}\n", b64("sk-other"), b64(token));
    c.stage(&nonce, &[("ssh/id_ed25519", "KEY"), ("ca/corp.pem", PEM)], &env);
    // The first clone is done: `ok` cloned, `bad` failed (no credentials then).
    git(tmp.path(), &["clone", "-q", remote.to_str().unwrap(), c.p("ws/ok").to_str().unwrap()]);
    fs::write(c.p("clone/repos"), "ok\tcloning\nok\tcloned\nbad\tcloning\nbad\tfailed\n").unwrap();
    fs::write(c.p("clone/done"), "").unwrap();
    fs::write(
        c.p("run/repos.json"),
        format!(
            r#"[{{"url":"{r}","branch":null,"folder":"ok"}},{{"url":"{r}","branch":"dev","folder":"bad"}}]"#,
            r = remote.display()
        ),
    )
    .unwrap();

    let out = c.run(&[&nonce]);
    let log = text(&out);
    assert!(out.status.success(), "{log}");
    assert!(log.contains("credentials applied"), "{log}");
    assert!(!c.p(&format!("shm/connect/{nonce}")).exists(), "the apply removes its staging");
    assert!(!log.contains(token) && !log.contains("sk-other"), "no value is printed: {log}");

    // CA, as the entrypoint installs it.
    assert_eq!(fs::read_to_string(c.p("store/triple-c-corp.crt")).unwrap(), PEM);
    assert_eq!(fs::read_to_string(c.p("update-calls")).unwrap(), "ran\n");
    // SSH merged into ~/.ssh.
    assert_eq!(fs::read_to_string(c.p("home/.ssh/id_ed25519")).unwrap(), "KEY");
    assert_eq!(mode(&c.p("home/.ssh/id_ed25519")), 0o600);
    // The git credential helper.
    let cred = fs::read_to_string(c.p("home/.git-credentials")).unwrap();
    assert!(cred.contains(&format!("https://oauth2:{token}@github.com")), "{cred}");
    // The marker the waiting clone starts on.
    assert!(c.p("run/connected").exists());

    // Only `bad` is cloned again, in the background.
    c.wait_done();
    let repos = fs::read_to_string(c.p("clone/repos")).unwrap();
    assert_eq!(
        repos,
        "ok\tcloning\nok\tcloned\nbad\tcloning\nbad\tfailed\nbad\tcloning\nbad\tcloned\n",
        "{}",
        fs::read_to_string(c.p("log")).unwrap()
    );
    assert_eq!(git(&c.p("ws/bad"), &["rev-parse", "--abbrev-ref", "HEAD"]), "dev");
    let clone_log = fs::read_to_string(c.p("log")).unwrap();
    assert!(clone_log.contains("cloning again with the project's credentials: bad"), "{clone_log}");
    assert!(!clone_log.contains(token), "{clone_log}");

    // A new connect with the same credentials: nothing failed, nothing
    // re-cloned, same state, the CA not reinstalled.
    let out = c.connect(&[("ssh/id_ed25519", "KEY"), ("ca/corp.pem", PEM)], &env);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_to_string(c.p("clone/repos")).unwrap(), repos);
    assert!(c.p("clone/done").exists());
    assert_eq!(fs::read_to_string(c.p("home/.git-credentials")).unwrap(), cred);
    assert_eq!(fs::read_to_string(c.p("home/.ssh/id_ed25519")).unwrap(), "KEY");
    assert_eq!(fs::read_to_string(c.p("update-calls")).unwrap(), "ran\n", "the CA stamp skips a reinstall");
}

#[test]
fn a_failed_ca_move_keeps_the_ca_installed_before() {
    let tmp = tempfile::tempdir().unwrap();
    let c = Connect::new(tmp.path());
    let out = c.connect(&[("ca/corp.pem", PEM)], "");
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_to_string(c.p("shm/connect-ca/corp.pem")).unwrap(), PEM);

    // Every move of a staged `ca` dir fails: the old files stay in place and
    // stay installed.
    let pem2 = "-----BEGIN CERTIFICATE-----\nMIIC\n-----END CERTIFICATE-----\n";
    write_script(
        &c.p("bin"),
        "mv",
        "#!/bin/sh\nfor a; do case \"$a\" in */ca) exit 1 ;; esac; done\nexec /usr/bin/mv \"$@\"\n",
    );
    let out = c.connect(&[("ca/corp.pem", pem2)], "");
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("keeping the ones from before"), "{}", text(&out));
    assert_eq!(fs::read_to_string(c.p("shm/connect-ca/corp.pem")).unwrap(), PEM);
    assert_eq!(fs::read_to_string(c.p("store/triple-c-corp.crt")).unwrap(), PEM);

    // With the moves working again the new files are swapped in, nothing left over.
    fs::remove_file(c.p("bin/mv")).unwrap();
    let out = c.connect(&[("ca/corp.pem", pem2)], "");
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_to_string(c.p("shm/connect-ca/corp.pem")).unwrap(), pem2);
    assert_eq!(fs::read_to_string(c.p("store/triple-c-corp.crt")).unwrap(), pem2);
    assert!(!c.p("shm/connect-ca.new").exists() && !c.p("shm/connect-ca.old").exists());

    // A connect with no CA removes it.
    let out = c.connect(&[], "");
    assert!(out.status.success(), "{}", text(&out));
    assert!(!c.p("shm/connect-ca").exists());
    assert!(!c.p("store/triple-c-corp.crt").exists());
}

#[test]
fn apply_connect_with_nothing_configured_changes_nothing_and_leaves_a_waiting_clone_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let c = Connect::new(tmp.path());
    let nonce = c.prepare();
    // The first clone is still waiting for this connect: no `done`, no
    // `no-credentials`.
    fs::write(c.p("run/repos.json"), "[]").unwrap();
    let out = c.run(&[&nonce]);
    let log = text(&out);
    assert!(out.status.success(), "{log}");
    assert!(!log.contains("warning"), "{log}");
    assert!(!c.p("home/.ssh").exists() && !c.p("home/.git-credentials").exists());
    assert!(!c.p("update-calls").exists());
    assert!(c.p("run/connected").exists());
    assert!(!c.p("clone/done").exists(), "nothing retried");
    assert_eq!(fs::read_to_string(c.p("log")).unwrap(), "");
}

#[test]
fn apply_connect_retries_a_clone_that_started_without_credentials_once_it_is_done() {
    let tmp = tempfile::tempdir().unwrap();
    let c = Connect::new(tmp.path());
    let remote = bare_remote(tmp.path(), "remote");
    let nonce = c.prepare();
    fs::write(
        c.p("run/repos.json"),
        format!(r#"[{{"url":"{}","branch":null,"folder":"late"}}]"#, remote.display()),
    )
    .unwrap();
    // Still cloning without credentials (the wait timed out).
    fs::write(c.p("clone/no-credentials"), "").unwrap();
    fs::write(c.p("clone/repos"), "late\tcloning\n").unwrap();
    let out = c.run(&[&nonce]);
    assert!(out.status.success(), "{}", text(&out));
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(!c.p("ws/late").exists(), "nothing until the running clone is done");
    // That clone ends, failed.
    fs::write(c.p("clone/repos"), "late\tcloning\nlate\tfailed\n").unwrap();
    fs::write(c.p("clone/done"), "").unwrap();
    for _ in 0..200 {
        if fs::read_to_string(c.p("clone/repos")).unwrap().ends_with("late\tcloned\n") && c.p("clone/done").exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(
        fs::read_to_string(c.p("clone/repos")).unwrap().ends_with("late\tcloning\nlate\tcloned\n"),
        "{}",
        fs::read_to_string(c.p("log")).unwrap()
    );
    assert!(c.p("ws/late/README").exists());
    assert!(!c.p("clone/no-credentials").exists());
}

#[test]
fn a_running_re_clone_never_holds_the_apply_lock_and_prepare_waits_for_an_apply() {
    let tmp = tempfile::tempdir().unwrap();
    let c = Connect::new(tmp.path());
    // A re-clone that takes a while.
    let mut lib = fs::read_to_string(c.p("lib.sh")).unwrap();
    lib.push_str("\nclone_repos() { sleep 6; }\n");
    fs::write(c.p("lib.sh"), lib).unwrap();
    let nonce = c.prepare();
    fs::write(c.p("run/repos.json"), r#"[{"url":"x","branch":null,"folder":"bad"}]"#).unwrap();
    fs::write(c.p("clone/repos"), "bad\tcloning\nbad\tfailed\n").unwrap();
    fs::write(c.p("clone/done"), "").unwrap();
    assert!(c.run(&[&nonce]).status.success());
    assert!(!c.p("clone/done").exists(), "the re-clone is running");
    // The next connect goes straight through.
    let started = std::time::Instant::now();
    let out = c.connect(&[], "");
    assert!(out.status.success(), "{}", text(&out));
    assert!(!text(&out).contains("another apply"), "{}", text(&out));
    assert!(started.elapsed() < std::time::Duration::from_secs(4), "{:?}", started.elapsed());
    c.wait_done();

    // An apply in progress (the lock held) holds a prepare back.
    let lock = c.p("shm/apply-connect.lock");
    let mut holder = Command::new("flock").arg(&lock).args(["sleep", "2"]).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let started = std::time::Instant::now();
    assert!(c.run(&["--prepare"]).status.success());
    assert!(started.elapsed() >= std::time::Duration::from_millis(1200), "{:?}", started.elapsed());
    holder.wait().unwrap();
}

#[test]
fn under_copy_at_connect_hybrid_keeps_ssh_off_efs() {
    let tmp = tempfile::tempdir().unwrap();
    let (home, mnt) = hybrid_dirs(tmp.path());
    fs::create_dir_all(home.join(".ssh")).unwrap();
    fs::create_dir_all(home.join(".claude")).unwrap();
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("id_ed25519"), "KEY").unwrap();
    let script = format!(
        "{}\ntriplec_aws=1\ntriplec_connect=1\nlink_persistent_home \"$1\" \"$2\" \"\"\nmerge_host_ssh \"$3\"\n",
        aws_blocks(&["aws_home", "sync_host_ssh", "merge_host_ssh"])
            .replace("/home/claude", home.to_str().unwrap())
            .replace("triplec_home_mnt=/mnt/triple-c-home", &format!("triplec_home_mnt='{}'", mnt.display()))
            .replace("chown -R claude:claude", ": chown")
    );
    let out = bash(&script, &[&home, &mnt, &src], &[], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("~/.ssh does not"), "{}", text(&out));
    assert!(fs::symlink_metadata(home.join(".claude")).unwrap().file_type().is_symlink());
    let ssh = fs::symlink_metadata(home.join(".ssh")).unwrap();
    assert!(ssh.is_dir() && !ssh.file_type().is_symlink(), "~/.ssh is a local directory");
    assert_eq!(fs::read_to_string(home.join(".ssh/id_ed25519")).unwrap(), "KEY");
    assert!(!mnt.join(".ssh").exists(), "nothing written to EFS");
}

#[test]
fn two_connects_at_once_never_mix_their_files() {
    // Two machines connecting at the same moment: both prepare, then both
    // upload, then both apply. Each applies its own set; the later wins.
    let tmp = tempfile::tempdir().unwrap();
    let c = Connect::new(tmp.path());
    let a = c.prepare();
    let b = c.prepare();
    assert_ne!(a, b);
    c.stage(&a, &[("ssh/id_a", "A"), ("ssh/config", "Host a\n")], &format!("GIT_TOKEN={}\n", b64("tok-a")));
    c.stage(&b, &[("ssh/id_b", "B"), ("ssh/config", "Host b\n")], &format!("GIT_TOKEN={}\n", b64("tok-b")));
    for n in [&a, &b] {
        let out = c.run(&[n]);
        assert!(out.status.success(), "{}", text(&out));
    }
    assert_eq!(fs::read_to_string(c.p("home/.ssh/config")).unwrap(), "Host b\n");
    let cred = fs::read_to_string(c.p("home/.git-credentials")).unwrap_or_else(|e| panic!("{e}: {:?}", fs::read_dir(c.p("home")).unwrap().map(|e| e.unwrap().file_name()).collect::<Vec<_>>()));
    assert!(cred.contains("tok-b"), "{cred}");
    // B's set as a whole: A's key, merged first, is dropped again by the
    // merge's own bookkeeping (B no longer provides it).
    assert_eq!(fs::read_to_string(c.p("home/.ssh/id_b")).unwrap(), "B");
    assert!(!c.p("home/.ssh/id_a").exists());
    assert_eq!(fs::read_dir(c.p("shm/connect")).unwrap().count(), 0, "both staging dirs gone");
    // A nonce that is not one, or not prepared, is refused.
    assert_eq!(c.run(&["../../etc"]).status.code(), Some(2));
    assert_eq!(c.run(&[&a]).status.code(), Some(1));
}
