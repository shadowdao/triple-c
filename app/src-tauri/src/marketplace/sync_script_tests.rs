//! Runs the real `sync.sh` against a throwaway `$HOME`, with a stub `claude`
//! on `PATH` that records its arguments. Skipped when `jq` or `tar` is missing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use super::sync::SYNC_SCRIPT;
use crate::models::marketplace::SyncReport;

const C1: &str = "1111111111111111111111111111111111111111";
const C2: &str = "2222222222222222222222222222222222222222";
const SLUG: &str = "team-tools-m1aaaaaa";

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

struct Env {
    _root: tempfile::TempDir,
    home: PathBuf,
    incoming: PathBuf,
    stub_dir: PathBuf,
    log: PathBuf,
    script: PathBuf,
    lock: PathBuf,
}

fn env() -> Option<Env> {
    if !have("jq") || !have("tar") {
        eprintln!("skipping: jq or tar is not installed");
        return None;
    }
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let incoming = root.path().join("incoming");
    let stub_dir = root.path().join("bin");
    for d in [&home, &incoming, &stub_dir] {
        fs::create_dir_all(d).unwrap();
    }
    let log = root.path().join("claude.log");
    let stub = stub_dir.join("claude");
    fs::write(
        &stub,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$CLAUDE_LOG\"\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let script = root.path().join("sync.sh");
    fs::write(&script, SYNC_SCRIPT).unwrap();
    let lock = root.path().join("lock");
    Some(Env {
        home,
        incoming,
        stub_dir,
        log,
        script,
        lock,
        _root: root,
    })
}

/// Write `payload.tar` into the incoming dir: `files` plus `manifest.json`.
fn payload(env: &Env, files: &[(&str, &str, bool)], manifest: Value) {
    let mut b = tar::Builder::new(Vec::new());
    let mut add = |path: &str, data: &[u8], exec: bool| {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(if exec { 0o755 } else { 0o644 });
        h.set_entry_type(tar::EntryType::Regular);
        b.append_data(&mut h, path, data).unwrap();
    };
    for (path, text, exec) in files {
        add(path, text.as_bytes(), *exec);
    }
    add("manifest.json", manifest.to_string().as_bytes(), false);
    fs::write(env.incoming.join("payload.tar"), b.into_inner().unwrap()).unwrap();
}

fn run(env: &Env) -> SyncReport {
    run_with(env, "sh")
}

/// Run the script under a specific shell (`sh` is dash on Ubuntu).
fn run_with(env: &Env, shell: &str) -> SyncReport {
    run_full(env, shell, &[])
}

/// Run the script with extra environment variables.
fn run_full(env: &Env, shell: &str, extra: &[(&str, &str)]) -> SyncReport {
    let out = Command::new(shell)
        .arg(&env.script)
        .env_clear()
        .envs(extra.iter().copied())
        .env("HOME", &env.home)
        .env(
            "PATH",
            format!("{}:/usr/local/bin:/usr/bin:/bin", env.stub_dir.display()),
        )
        .env("MARKETPLACE_INCOMING", &env.incoming)
        .env("MARKETPLACE_LOCK", &env.lock)
        .env("CLAUDE_LOG", &env.log)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "script failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let last = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .expect("a report line");
    serde_json::from_str(last).unwrap_or_else(|e| panic!("bad report {last:?}: {e}"))
}

fn claude_log(env: &Env) -> Vec<String> {
    fs::read_to_string(&env.log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
}

fn hook_settings() -> Value {
    json!({ "Stop": [{ "hooks": [{ "type": "command",
        "command": "/home/claude/.claude/triple-c/hooks/notify-on-stop/notify.sh" }] }] })
}

fn all_kinds(commit: &str) -> (Vec<(&'static str, &'static str, bool)>, Value) {
    (
        vec![
            ("agents/code-reviewer.md", "agent body\n", false),
            ("skills/example-skill/SKILL.md", "skill body\n", false),
            ("commands/example-command.md", "command body\n", false),
            ("hooks/notify-on-stop/hook.json", "{}", false),
            (
                "hooks/notify-on-stop/notify.sh",
                "#!/bin/sh\necho hi\n",
                true,
            ),
        ],
        json!({ "version": 1, "plugin_marketplaces": [], "items": [
            { "kind": "agent", "key": "code-reviewer", "marketplace": "m1", "commit": commit, "file": "agents/code-reviewer.md" },
            { "kind": "skill", "key": "example-skill", "marketplace": "m1", "commit": commit, "dir": "skills/example-skill" },
            { "kind": "command", "key": "example-command", "marketplace": "m1", "commit": commit, "file": "commands/example-command.md" },
            { "kind": "hook", "key": "notify-on-stop", "marketplace": "m1", "commit": commit, "dir": "hooks/notify-on-stop", "settings": hook_settings() }
        ]}),
    )
}

fn empty_manifest() -> Value {
    json!({ "version": 1, "items": [], "plugin_marketplaces": [] })
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[test]
fn sync_installs_all_kinds() {
    let Some(env) = env() else { return };
    let (files, manifest) = all_kinds(C1);
    payload(&env, &files, manifest);

    let r = run(&env);

    assert_eq!(r.errors, Vec::<String>::new());
    assert_eq!(
        sorted(r.installed),
        vec![
            "agent:code-reviewer",
            "command:example-command",
            "hook:notify-on-stop",
            "skill:example-skill"
        ]
    );
    let claude = env.home.join(".claude");
    assert_eq!(
        fs::read_to_string(claude.join("agents/code-reviewer.md")).unwrap(),
        "agent body\n"
    );
    assert!(claude.join("skills/example-skill/SKILL.md").is_file());
    assert!(claude.join("commands/example-command.md").is_file());
    let script = claude.join("triple-c/hooks/notify-on-stop/notify.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            fs::metadata(&script).unwrap().permissions().mode() & 0o111,
            0,
            "hook script must stay executable"
        );
    }
    assert_eq!(
        read_json(&claude.join("settings.json"))["hooks"],
        hook_settings()
    );
    assert!(
        !env.incoming.join("payload.tar").exists(),
        "payload is consumed"
    );

    // A second identical run is a no-op in the report.
    payload(&env, &all_kinds(C1).0, all_kinds(C1).1);
    let again = run(&env);
    assert!(
        again.installed.is_empty() && again.updated.is_empty() && again.removed.is_empty(),
        "{again:?}"
    );
    assert_eq!(
        read_json(&claude.join("settings.json"))["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn sync_updates_report_changed_commits() {
    let Some(env) = env() else { return };
    let (files, manifest) = all_kinds(C1);
    payload(&env, &files, manifest);
    run(&env);
    let (files, manifest) = all_kinds(C2);
    payload(&env, &files, manifest);

    let r = run(&env);

    assert_eq!(r.updated.len(), 4, "{r:?}");
    assert!(r.installed.is_empty());
}

#[test]
fn sync_removes_deselected() {
    let Some(env) = env() else { return };
    let (files, manifest) = all_kinds(C1);
    payload(&env, &files, manifest);
    run(&env);
    payload(&env, &[], empty_manifest());

    let r = run(&env);

    assert_eq!(
        sorted(r.removed),
        vec![
            "agent:code-reviewer",
            "command:example-command",
            "hook:notify-on-stop",
            "skill:example-skill"
        ]
    );
    let claude = env.home.join(".claude");
    assert!(!claude.join("agents/code-reviewer.md").exists());
    assert!(!claude.join("skills/example-skill").exists());
    assert!(!claude.join("commands/example-command.md").exists());
    assert!(!claude.join("triple-c/hooks/notify-on-stop").exists());
    assert_eq!(read_json(&claude.join("settings.json")).get("hooks"), None);
}

#[test]
fn sync_skips_user_owned_agent() {
    let Some(env) = env() else { return };
    let mine = env.home.join(".claude/agents/code-reviewer.md");
    fs::create_dir_all(mine.parent().unwrap()).unwrap();
    fs::write(&mine, "mine\n").unwrap();
    let (files, manifest) = all_kinds(C1);
    payload(&env, &files, manifest);

    let r = run(&env);

    assert_eq!(r.skipped.len(), 1, "{r:?}");
    assert_eq!(r.skipped[0].item, "agent:code-reviewer");
    assert!(
        r.skipped[0]
            .reason
            .contains("was not installed by Triple-C"),
        "{}",
        r.skipped[0].reason
    );
    assert_eq!(fs::read_to_string(&mine).unwrap(), "mine\n");

    // Deselecting everything must not delete the user's own file either.
    payload(&env, &[], empty_manifest());
    let r = run(&env);
    assert!(!r.removed.contains(&"agent:code-reviewer".to_string()));
    assert_eq!(fs::read_to_string(&mine).unwrap(), "mine\n");
}

#[test]
fn sync_preserves_user_hooks() {
    let Some(env) = env() else { return };
    let settings_path = env.home.join(".claude/settings.json");
    fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    let original = json!({
        "model": "opus",
        "hooks": {
            "Stop": [{ "hooks": [{ "type": "command", "command": "echo mine" }] }],
            "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo pre" }] }]
        }
    });
    fs::write(
        &settings_path,
        serde_json::to_string_pretty(&original).unwrap(),
    )
    .unwrap();

    let (files, manifest) = all_kinds(C1);
    payload(&env, &files, manifest);
    run(&env);
    let merged = read_json(&settings_path);
    assert_eq!(merged["model"], "opus");
    assert_eq!(
        merged["hooks"]["PreToolUse"],
        original["hooks"]["PreToolUse"]
    );
    assert_eq!(
        merged["hooks"]["Stop"][0], original["hooks"]["Stop"][0],
        "user hook stays first"
    );
    assert_eq!(merged["hooks"]["Stop"][1], hook_settings()["Stop"][0]);

    // An update with a changed hook entry replaces only ours.
    let (files, mut manifest) = all_kinds(C2);
    manifest["items"][3]["settings"]["Stop"][0]["hooks"][0]["timeout"] = json!(5);
    payload(&env, &files, manifest);
    run(&env);
    let updated = read_json(&settings_path);
    assert_eq!(updated["hooks"]["Stop"].as_array().unwrap().len(), 2);
    assert_eq!(updated["hooks"]["Stop"][0], original["hooks"]["Stop"][0]);
    assert_eq!(updated["hooks"]["Stop"][1]["hooks"][0]["timeout"], 5);

    // Uninstalling everything restores the user's settings exactly.
    payload(&env, &[], empty_manifest());
    run(&env);
    assert_eq!(read_json(&settings_path), original);
}

#[test]
fn sync_plugin_calls() {
    let Some(env) = env() else { return };
    let files = [
        (
            "plugins/team-tools-m1aaaaaa/.claude-plugin/marketplace.json",
            r#"{"name":"triple-c-team-tools-m1aaaaaa","owner":{"name":"Triple-C"},"plugins":[{"name":"example-plugin","source":"./example-plugin"}]}"#,
            false,
        ),
        (
            "plugins/team-tools-m1aaaaaa/example-plugin/.claude-plugin/plugin.json",
            r#"{"name":"example-plugin"}"#,
            false,
        ),
    ];
    let manifest = |commit: &str| {
        json!({ "version": 1,
            "items": [{ "kind": "plugin", "key": "example-plugin", "marketplace": "m1", "commit": commit, "slug": SLUG }],
            "plugin_marketplaces": [{ "slug": SLUG, "dir": format!("plugins/{SLUG}"), "plugins": ["example-plugin"] }] })
    };
    let tree = env.home.join(".claude/triple-c/plugins").join(SLUG);

    payload(&env, &files, manifest(C1));
    let r = run(&env);
    assert_eq!(r.installed, vec!["plugin:example-plugin"]);
    assert_eq!(
        claude_log(&env),
        vec![
            format!("plugin marketplace add {}", tree.display()),
            format!("plugin install example-plugin@triple-c-{SLUG}"),
        ]
    );
    assert!(tree.join(".claude-plugin/marketplace.json").is_file());

    // Same commit again: catalog refreshed, nothing reinstalled.
    fs::remove_file(&env.log).unwrap();
    payload(&env, &files, manifest(C1));
    run(&env);
    assert_eq!(
        claude_log(&env),
        vec![format!("plugin marketplace update triple-c-{SLUG}")]
    );

    // New commit: uninstall + install.
    fs::remove_file(&env.log).unwrap();
    payload(&env, &files, manifest(C2));
    let r = run(&env);
    assert_eq!(r.updated, vec!["plugin:example-plugin"]);
    assert_eq!(
        claude_log(&env),
        vec![
            format!("plugin marketplace update triple-c-{SLUG}"),
            format!("plugin uninstall example-plugin@triple-c-{SLUG}"),
            format!("plugin install example-plugin@triple-c-{SLUG}"),
        ]
    );

    // Deselected: uninstall, drop the registration and the tree.
    fs::remove_file(&env.log).unwrap();
    payload(&env, &[], empty_manifest());
    let r = run(&env);
    assert_eq!(r.removed, vec!["plugin:example-plugin"]);
    assert_eq!(
        claude_log(&env),
        vec![
            format!("plugin uninstall example-plugin@triple-c-{SLUG}"),
            format!("plugin marketplace remove triple-c-{SLUG}"),
        ]
    );
    assert!(!tree.exists());
}

#[test]
fn sync_rejects_bad_keys() {
    let Some(env) = env() else { return };
    payload(
        &env,
        &[("agents/x.md", "x", false)],
        json!({ "version": 1, "plugin_marketplaces": [], "items": [
            { "kind": "agent", "key": "../../evil", "marketplace": "m1", "commit": C1 },
            { "kind": "agent", "key": "-rf", "marketplace": "m1", "commit": C1 },
            { "kind": "agent", "key": "ok", "marketplace": "m1", "commit": "not-a-sha" }
        ]}),
    );
    let r = run(&env);
    assert_eq!(r.skipped.len(), 3, "{r:?}");
    assert!(r.installed.is_empty());
    assert!(!env.home.join("evil.md").exists());
}

#[test]
fn a_missing_payload_is_reported_not_fatal() {
    let Some(env) = env() else { return };
    let r = run(&env);
    assert_eq!(r.errors, vec!["no payload was uploaded"]);
}

#[test]
fn sync_keeps_settings_json_private() {
    // Pre-flight N11: the entrypoint keeps settings.json at 0600; a hook
    // merge must not leave it world-readable.
    let Some(env) = env() else { return };
    let settings_path = env.home.join(".claude/settings.json");
    let (files, manifest) = all_kinds(C1);
    payload(&env, &files, manifest);
    run(&env);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&settings_path), 0o600, "created by the merge");

        fs::set_permissions(&settings_path, fs::Permissions::from_mode(0o644)).unwrap();
        payload(&env, &[], empty_manifest());
        run(&env);
        assert_eq!(
            mode(&settings_path),
            0o600,
            "rewritten by the uninstall merge"
        );
    }
}

#[test]
fn sync_runs_under_bash_and_dash() {
    let Some(env) = env() else { return };
    for shell in ["bash", "dash"] {
        if !Path::new("/bin").join(shell).exists() && !have(shell) {
            eprintln!("skipping {shell}: not installed");
            continue;
        }
        let (files, manifest) = all_kinds(C1);
        payload(&env, &files, manifest);
        let r = run_with(&env, shell);
        assert_eq!(r.errors, Vec::<String>::new(), "{shell}");
        assert_eq!(r.installed.len(), 4, "{shell}: {r:?}");
        payload(&env, &[], empty_manifest());
        let r = run_with(&env, shell);
        assert_eq!(r.removed.len(), 4, "{shell}: {r:?}");
    }
}

#[test]
fn sync_never_interpolates_hostile_keys() {
    let Some(env) = env() else { return };
    let canary = env.home.join("pwned");
    let evil = format!("a$(touch {})", canary.display());
    let evil_tab = "a\tb";
    let evil_nl = "ok\nagent\tgood";
    let evil_slug = format!("s;touch {}", canary.display());
    payload(
        &env,
        &[("agents/a.md", "x", false)],
        json!({ "version": 1,
        "plugin_marketplaces": [{ "slug": evil_slug, "dir": "plugins/x", "plugins": [] }],
        "items": [
            { "kind": "agent", "key": evil, "marketplace": "m1", "commit": C1 },
            { "kind": "agent", "key": evil_tab, "marketplace": "m1", "commit": C1 },
            { "kind": "agent", "key": evil_nl, "marketplace": "m1", "commit": C1 },
            { "kind": "agent", "key": "a".repeat(65), "marketplace": "m1", "commit": C1 },
            { "kind": "agent$(id)", "key": "a", "marketplace": "m1", "commit": C1 },
            { "kind": "plugin", "key": "p", "marketplace": "m1", "commit": C1, "slug": evil_slug }
        ]}),
    );
    let r = run(&env);
    assert!(r.installed.is_empty(), "{r:?}");
    assert_eq!(r.skipped.len(), 5, "{r:?}");
    assert!(!canary.exists(), "a manifest value was executed");
    assert!(claude_log(&env).is_empty(), "{:?}", claude_log(&env));
    assert!(!env.home.join(".claude/agents").exists());
}

#[test]
fn sync_rejects_a_payload_with_symlinks() {
    let Some(env) = env() else { return };
    let outside = env.home.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Symlink);
    h.set_size(0);
    h.set_mode(0o777);
    b.append_link(&mut h, "skills/example-skill", &outside)
        .unwrap();
    let manifest = json!({ "version": 1, "plugin_marketplaces": [], "items": [
        { "kind": "skill", "key": "example-skill", "marketplace": "m1", "commit": C1, "dir": "skills/example-skill" }
    ]})
    .to_string();
    let mut h = tar::Header::new_gnu();
    h.set_size(manifest.len() as u64);
    h.set_mode(0o644);
    b.append_data(&mut h, "manifest.json", manifest.as_bytes())
        .unwrap();
    fs::write(env.incoming.join("payload.tar"), b.into_inner().unwrap()).unwrap();

    let r = run(&env);

    assert!(r.installed.is_empty(), "{r:?}");
    assert_eq!(r.errors.len(), 1, "{r:?}");
    assert!(r.errors[0].contains("symbolic link"), "{r:?}");
    assert!(!env.home.join(".claude/skills/example-skill").exists());
}

#[test]
fn sync_skips_user_owned_skill_and_command() {
    let Some(env) = env() else { return };
    let claude = env.home.join(".claude");
    let skill = claude.join("skills/example-skill/SKILL.md");
    let command = claude.join("commands/example-command.md");
    for p in [&skill, &command] {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "mine\n").unwrap();
    }
    let (files, manifest) = all_kinds(C1);
    payload(&env, &files, manifest);

    let r = run(&env);

    let skipped = sorted(r.skipped.iter().map(|s| s.item.clone()).collect());
    assert_eq!(
        skipped,
        vec!["command:example-command", "skill:example-skill"],
        "{r:?}"
    );
    assert_eq!(
        sorted(r.installed),
        vec!["agent:code-reviewer", "hook:notify-on-stop"]
    );
    assert_eq!(fs::read_to_string(&skill).unwrap(), "mine\n");
    assert_eq!(fs::read_to_string(&command).unwrap(), "mine\n");

    payload(&env, &[], empty_manifest());
    let r = run(&env);
    assert_eq!(
        sorted(r.removed),
        vec!["agent:code-reviewer", "hook:notify-on-stop"]
    );
    assert_eq!(fs::read_to_string(&skill).unwrap(), "mine\n");
    assert_eq!(fs::read_to_string(&command).unwrap(), "mine\n");
}

// ── Review fix round 1 ──────────────────────────────────────────────────────

fn install_all(env: &Env) {
    let (files, manifest) = all_kinds(C1);
    payload(env, &files, manifest);
    let r = run(env);
    assert_eq!(r.installed.len(), 4, "{r:?}");
}

fn assert_all_installed(env: &Env) {
    let claude = env.home.join(".claude");
    assert!(claude.join("agents/code-reviewer.md").is_file());
    assert!(claude.join("skills/example-skill/SKILL.md").is_file());
    assert!(claude.join("commands/example-command.md").is_file());
    assert!(claude
        .join("triple-c/hooks/notify-on-stop/notify.sh")
        .is_file());
}

#[test]
fn sync_malformed_item_neither_aborts_nor_removes() {
    let Some(env) = env() else { return };
    install_all(&env);

    // Malformed entries first, then the valid ones; the still-selected agent
    // has a non-string commit.
    let (files, mut manifest) = all_kinds(C1);
    manifest["items"][0]["commit"] = json!(7);
    let items = manifest["items"].as_array().unwrap().clone();
    let mut all = vec![
        json!({ "kind": "agent", "key": { "x": 1 }, "marketplace": "m1", "commit": C1 }),
        json!(5),
        json!({ "kind": ["agent"], "key": "k", "marketplace": "m1", "commit": C1 }),
        json!({ "kind": "plugin", "key": { "y": 1 }, "marketplace": "m1", "commit": C1, "slug": SLUG }),
    ];
    all.extend(items);
    manifest["items"] = Value::Array(all);
    payload(&env, &files, manifest);

    let r = run(&env);

    assert!(r.removed.is_empty(), "{r:?}");
    assert!(r.errors.is_empty(), "{r:?}");
    assert_all_installed(&env);
    let skipped = r
        .skipped
        .iter()
        .map(|s| s.item.as_str())
        .collect::<Vec<_>>();
    assert!(skipped.contains(&"agent:code-reviewer"), "{r:?}");
    assert_eq!(r.skipped.len(), 5, "{r:?}");

    // The carried-forward record is still owned: deselecting removes it.
    payload(&env, &[], empty_manifest());
    let r = run(&env);
    assert_eq!(r.removed.len(), 4, "{r:?}");
}

#[test]
fn sync_structurally_bad_manifest_removes_nothing() {
    let Some(env) = env() else { return };
    install_all(&env);
    for bad in [
        json!({ "version": 1 }),
        json!({ "version": 1, "items": {}, "plugin_marketplaces": [] }),
        json!({ "version": 1, "items": [], "plugin_marketplaces": "x" }),
    ] {
        payload(&env, &[], bad.clone());
        let r = run(&env);
        assert!(r.removed.is_empty(), "{bad}: {r:?}");
        assert_eq!(r.errors.len(), 1, "{bad}: {r:?}");
        assert_all_installed(&env);
    }
    // State survived: a real deselection still removes everything.
    payload(&env, &[], empty_manifest());
    assert_eq!(run(&env).removed.len(), 4);
}

#[test]
fn sync_treats_blank_settings_as_empty() {
    let Some(env) = env() else { return };
    let settings_path = env.home.join(".claude/settings.json");
    fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    fs::write(&settings_path, "  \n\t\n").unwrap();
    let (files, manifest) = all_kinds(C1);
    payload(&env, &files, manifest);

    let r = run(&env);

    assert!(r.errors.is_empty(), "{r:?}");
    assert!(
        r.installed.contains(&"hook:notify-on-stop".to_string()),
        "{r:?}"
    );
    assert_eq!(read_json(&settings_path)["hooks"], hook_settings());
}

#[test]
fn sync_does_not_report_hooks_it_could_not_wire() {
    let Some(env) = env() else { return };
    let settings_path = env.home.join(".claude/settings.json");
    fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    for bad in ["[1]", "{\"a\":", "{} {}"] {
        fs::write(&settings_path, bad).unwrap();
        let (files, manifest) = all_kinds(C1);
        payload(&env, &files, manifest);
        let r = run(&env);
        assert!(
            !r.installed.contains(&"hook:notify-on-stop".to_string()),
            "{bad}: {r:?}"
        );
        assert_eq!(r.errors.len(), 1, "{bad}: {r:?}");
        assert_eq!(
            fs::read_to_string(&settings_path).unwrap(),
            bad,
            "left untouched"
        );
    }
    // Once settings.json is fixed the hook is installed for real.
    fs::write(&settings_path, "{}").unwrap();
    let (files, manifest) = all_kinds(C1);
    payload(&env, &files, manifest);
    let r = run(&env);
    assert_eq!(r.installed, vec!["hook:notify-on-stop"], "{r:?}");
    assert_eq!(read_json(&settings_path)["hooks"], hook_settings());
}

#[cfg(unix)]
#[test]
fn sync_skips_dangling_user_symlinks() {
    use std::os::unix::fs::symlink;
    let Some(env) = env() else { return };
    let claude = env.home.join(".claude");
    let agent = claude.join("agents/code-reviewer.md");
    let skill = claude.join("skills/example-skill");
    for p in [&agent, &skill] {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        symlink("/nonexistent/dotfiles/target", p).unwrap();
    }
    let (files, manifest) = all_kinds(C1);
    payload(&env, &files, manifest);

    let r = run(&env);

    let skipped = sorted(r.skipped.iter().map(|s| s.item.clone()).collect());
    assert_eq!(
        skipped,
        vec!["agent:code-reviewer", "skill:example-skill"],
        "{r:?}"
    );
    for p in [&agent, &skill] {
        assert_eq!(
            fs::read_link(p).unwrap(),
            Path::new("/nonexistent/dotfiles/target")
        );
    }
}

#[test]
fn sync_removal_never_uses_paths_from_state() {
    let Some(env) = env() else { return };
    install_all(&env);
    let claude = env.home.join(".claude");
    let keep = claude.join("keep.txt");
    fs::write(&keep, "keep").unwrap();
    let state_path = claude.join("triple-c/marketplace/state.json");
    let mut state = read_json(&state_path);
    state["items"]["agent:code-reviewer"]["path"] = json!(format!("{}/", claude.display()));
    state["items"]["skill:example-skill"]["path"] =
        json!(format!("{}/.claude", env.home.display()));
    state["items"]["command:example-command"]["path"] = json!(keep.display().to_string());
    fs::write(&state_path, state.to_string()).unwrap();

    payload(&env, &[], empty_manifest());
    let r = run(&env);

    assert_eq!(r.removed.len(), 4, "{r:?}");
    assert_eq!(fs::read_to_string(&keep).unwrap(), "keep");
    assert!(!claude.join("agents/code-reviewer.md").exists());
    assert!(!claude.join("skills/example-skill").exists());
    assert!(!claude.join("commands/example-command.md").exists());
}

#[cfg(unix)]
#[test]
fn sync_writes_through_a_symlinked_settings_json() {
    use std::os::unix::fs::symlink;
    let Some(env) = env() else { return };
    let dotfiles = env.home.join("dotfiles/settings.json");
    fs::create_dir_all(dotfiles.parent().unwrap()).unwrap();
    fs::write(&dotfiles, r#"{"model":"opus"}"#).unwrap();
    let settings_path = env.home.join(".claude/settings.json");
    fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    symlink(&dotfiles, &settings_path).unwrap();
    let (files, manifest) = all_kinds(C1);
    payload(&env, &files, manifest);

    let r = run(&env);

    assert!(r.errors.is_empty(), "{r:?}");
    assert!(
        fs::symlink_metadata(&settings_path)
            .unwrap()
            .file_type()
            .is_symlink(),
        "link kept"
    );
    let merged = read_json(&dotfiles);
    assert_eq!(merged["model"], "opus");
    assert_eq!(merged["hooks"], hook_settings());
}

#[test]
fn sync_reports_when_mktemp_fails() {
    let Some(env) = env() else { return };
    let r = run_full(&env, "sh", &[("TMPDIR", "/nonexistent/triple-c-tmp")]);
    assert_eq!(r.errors.len(), 1, "{r:?}");
}

#[cfg(unix)]
#[test]
fn sync_settings_move_failure_is_reported_and_not_recorded() {
    use std::os::unix::fs::PermissionsExt;
    let Some(env) = env() else { return };
    // An `mv` that refuses to replace settings.json and delegates otherwise.
    let mv = env.stub_dir.join("mv");
    fs::write(
        &mv,
        "#!/bin/sh\nfor a; do last=$a; done\ncase \"$last\" in */settings.json) exit 1 ;; esac\nexec /bin/mv \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&mv, fs::Permissions::from_mode(0o755)).unwrap();
    let (files, manifest) = all_kinds(C1);
    payload(&env, &files, manifest);

    let r = run(&env);

    assert_eq!(r.errors.len(), 1, "{r:?}");
    assert!(
        !r.installed.contains(&"hook:notify-on-stop".to_string()),
        "{r:?}"
    );
    let claude = env.home.join(".claude");
    assert!(!claude.join("settings.json").exists());
    let leftovers: Vec<_> = fs::read_dir(&claude)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
        .collect();
    assert!(leftovers.is_empty(), "temp file cleaned up");
    let state = read_json(&claude.join("triple-c/marketplace/state.json"));
    assert!(
        state["items"].get("hook:notify-on-stop").is_none(),
        "{state}"
    );
}

#[test]
fn sync_drops_state_records_without_a_kind_key_id() {
    let Some(env) = env() else { return };
    install_all(&env);
    let claude = env.home.join(".claude");
    let mine = [
        claude.join("skills/skill/SKILL.md"),
        claude.join("agents/agent.md"),
    ];
    for p in &mine {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "mine\n").unwrap();
    }
    let state_path = claude.join("triple-c/marketplace/state.json");
    let mut state = read_json(&state_path);
    for bogus in ["skill", "agent", "widget:x", "agent:a:b", "plugin:a:b"] {
        state["items"][bogus] = json!({ "commit": C1, "path": "/" });
    }
    fs::write(&state_path, state.to_string()).unwrap();

    payload(&env, &[], empty_manifest());
    let r = run(&env);

    assert_eq!(sorted(r.removed.clone()).len(), 4, "{r:?}");
    assert_eq!(r.errors.len(), 5, "{r:?}");
    assert!(claude_log(&env).is_empty(), "{:?}", claude_log(&env));
    for p in &mine {
        assert_eq!(fs::read_to_string(p).unwrap(), "mine\n", "{}", p.display());
    }
    let state = read_json(&state_path);
    assert_eq!(state["items"], json!({}), "bogus records dropped");
}

// ── Plugin state per marketplace (final review I1) ───────────────────────────

const SLUG_A: &str = "mp-aaaaaaaa";
const SLUG_B: &str = "mp-bbbbbbbb";

/// A payload in which each marketplace ships plugin `p` at its own commit.
fn shared_plugin_payload(env: &Env, slugs: &[(&str, &str)]) {
    let mut files: Vec<(String, String)> = Vec::new();
    let mut items = Vec::new();
    let mut groups = Vec::new();
    for (slug, commit) in slugs {
        files.push((
            format!("plugins/{slug}/.claude-plugin/marketplace.json"),
            format!(
                r#"{{"name":"triple-c-{slug}","owner":{{"name":"Triple-C"}},"plugins":[{{"name":"p","source":"./p"}}]}}"#
            ),
        ));
        files.push((
            format!("plugins/{slug}/p/.claude-plugin/plugin.json"),
            r#"{"name":"p"}"#.to_string(),
        ));
        items.push(json!({ "kind": "plugin", "key": "p", "marketplace": slug, "commit": commit, "slug": slug }));
        groups.push(json!({ "slug": slug, "dir": format!("plugins/{slug}"), "plugins": ["p"] }));
    }
    let refs: Vec<(&str, &str, bool)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str(), false))
        .collect();
    payload(
        env,
        &refs,
        json!({ "version": 1, "items": items, "plugin_marketplaces": groups }),
    );
}

fn nothing_reported(r: &SyncReport) -> bool {
    r.installed.is_empty() && r.updated.is_empty() && r.removed.is_empty() && r.errors.is_empty()
}

#[test]
fn two_marketplaces_sharing_a_plugin_name_reach_a_steady_state() {
    let Some(env) = env() else { return };

    shared_plugin_payload(&env, &[(SLUG_A, C1), (SLUG_B, C2)]);
    let r = run(&env);
    assert_eq!(r.installed, vec!["plugin:p", "plugin:p"], "{r:?}");
    assert!(r.errors.is_empty(), "{r:?}");

    // Nothing changed: no reinstall, nothing reported.
    for _ in 0..2 {
        fs::remove_file(&env.log).unwrap();
        shared_plugin_payload(&env, &[(SLUG_A, C1), (SLUG_B, C2)]);
        let r = run(&env);
        assert!(nothing_reported(&r), "{r:?}");
        assert_eq!(
            claude_log(&env),
            vec![
                format!("plugin marketplace update triple-c-{SLUG_A}"),
                format!("plugin marketplace update triple-c-{SLUG_B}"),
            ]
        );
    }

    // One marketplace's copy is removed: only that copy is uninstalled.
    fs::remove_file(&env.log).unwrap();
    shared_plugin_payload(&env, &[(SLUG_A, C1)]);
    let r = run(&env);
    assert_eq!(r.removed, vec!["plugin:p"], "{r:?}");
    assert!(r.installed.is_empty() && r.updated.is_empty(), "{r:?}");
    assert_eq!(
        claude_log(&env),
        vec![
            format!("plugin marketplace update triple-c-{SLUG_A}"),
            format!("plugin uninstall p@triple-c-{SLUG_B}"),
            format!("plugin marketplace remove triple-c-{SLUG_B}"),
        ]
    );

    // And the survivor stays put.
    fs::remove_file(&env.log).unwrap();
    shared_plugin_payload(&env, &[(SLUG_A, C1)]);
    let r = run(&env);
    assert!(nothing_reported(&r), "{r:?}");
    assert_eq!(
        claude_log(&env),
        vec![format!("plugin marketplace update triple-c-{SLUG_A}")]
    );
}

fn write_state(env: &Env, state: Value) -> PathBuf {
    let p = env.home.join(".claude/triple-c/marketplace/state.json");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, state.to_string()).unwrap();
    p
}

#[test]
fn legacy_plugin_records_are_migrated_not_reinstalled() {
    let Some(env) = env() else { return };
    // State as written by an earlier sync.sh: plugins keyed "plugin:<key>".
    let state_path = write_state(
        &env,
        json!({ "version": 1, "plugin_marketplaces": [SLUG_A],
            "items": { "plugin:p": { "commit": C1, "slug": SLUG_A } } }),
    );

    shared_plugin_payload(&env, &[(SLUG_A, C1)]);
    let r = run(&env);
    assert!(nothing_reported(&r), "{r:?}");
    assert_eq!(
        claude_log(&env),
        vec![format!("plugin marketplace update triple-c-{SLUG_A}")]
    );
    let items = read_json(&state_path)["items"].clone();
    assert!(items.get("plugin:p").is_none(), "{items}");
    assert_eq!(items[format!("plugin:{SLUG_A}/p")]["commit"], C1, "{items}");
}

#[test]
fn a_deselected_legacy_plugin_record_is_still_uninstalled() {
    let Some(env) = env() else { return };
    let state_path = write_state(
        &env,
        json!({ "version": 1, "plugin_marketplaces": [SLUG_A],
            "items": { "plugin:p": { "commit": C1, "slug": SLUG_A } } }),
    );

    payload(&env, &[], empty_manifest());
    let r = run(&env);
    assert_eq!(r.removed, vec!["plugin:p"], "{r:?}");
    assert_eq!(
        claude_log(&env),
        vec![
            format!("plugin uninstall p@triple-c-{SLUG_A}"),
            format!("plugin marketplace remove triple-c-{SLUG_A}"),
        ]
    );
    assert_eq!(read_json(&state_path)["items"], json!({}));
}

/// Final review M4: slugs moved from "<name>-<id8>" to "mp-<id8>". The first
/// sync after that installs under the new name, uninstalls the old copy and
/// drops the old registration; later syncs are quiet.
#[test]
fn a_slug_change_reinstalls_under_the_new_name_and_retires_the_old() {
    let Some(env) = env() else { return };
    shared_plugin_payload(&env, &[(SLUG, C1)]);
    assert_eq!(run(&env).installed, vec!["plugin:p"]);

    fs::remove_file(&env.log).unwrap();
    shared_plugin_payload(&env, &[(SLUG_A, C1)]);
    let r = run(&env);
    assert_eq!(r.installed, vec!["plugin:p"], "{r:?}");
    assert_eq!(r.removed, vec!["plugin:p"], "{r:?}");
    assert!(r.errors.is_empty(), "{r:?}");
    let tree = env.home.join(".claude/triple-c/plugins");
    assert_eq!(
        claude_log(&env),
        vec![
            format!("plugin marketplace add {}", tree.join(SLUG_A).display()),
            format!("plugin install p@triple-c-{SLUG_A}"),
            format!("plugin uninstall p@triple-c-{SLUG}"),
            format!("plugin marketplace remove triple-c-{SLUG}"),
        ]
    );
    assert!(!tree.join(SLUG).exists());

    fs::remove_file(&env.log).unwrap();
    shared_plugin_payload(&env, &[(SLUG_A, C1)]);
    let r = run(&env);
    assert!(nothing_reported(&r), "{r:?}");
}

// ── Host-side holds (final review M3) ────────────────────────────────────────

/// Everything `install_all` installed, plus plugin `p` from SLUG_A.
fn install_all_and_a_plugin(env: &Env) {
    let (base, mut manifest) = all_kinds(C1);
    let mut files: Vec<(String, String, bool)> = base
        .iter()
        .map(|(p, t, e)| (p.to_string(), t.to_string(), *e))
        .collect();
    files.push((
        format!("plugins/{SLUG_A}/.claude-plugin/marketplace.json"),
        format!(
            r#"{{"name":"triple-c-{SLUG_A}","owner":{{"name":"Triple-C"}},"plugins":[{{"name":"p","source":"./p"}}]}}"#
        ),
        false,
    ));
    files.push((
        format!("plugins/{SLUG_A}/p/.claude-plugin/plugin.json"),
        r#"{"name":"p"}"#.to_string(),
        false,
    ));
    manifest["items"].as_array_mut().unwrap().push(
        json!({ "kind": "plugin", "key": "p", "marketplace": "m1", "commit": C1, "slug": SLUG_A }),
    );
    manifest["plugin_marketplaces"] =
        json!([{ "slug": SLUG_A, "dir": format!("plugins/{SLUG_A}"), "plugins": ["p"] }]);
    let refs: Vec<(&str, &str, bool)> = files
        .iter()
        .map(|(p, t, e)| (p.as_str(), t.as_str(), *e))
        .collect();
    payload(env, &refs, manifest);
    let r = run(env);
    assert_eq!(r.installed.len(), 5, "{r:?}");
}

#[test]
fn held_items_are_kept_not_removed() {
    let Some(env) = env() else { return };
    install_all_and_a_plugin(&env);
    let settings_path = env.home.join(".claude/settings.json");
    let state_path = env.home.join(".claude/triple-c/marketplace/state.json");
    let state_before = read_json(&state_path)["items"].clone();

    // The host could build none of them this time (cache gone, say).
    fs::remove_file(&env.log).unwrap();
    let mut manifest = empty_manifest();
    manifest["held"] = json!([
        "agent:code-reviewer",
        "skill:example-skill",
        "command:example-command",
        "hook:notify-on-stop",
        format!("plugin:{SLUG_A}/p"),
    ]);
    payload(&env, &[], manifest);
    let r = run(&env);

    assert!(nothing_reported(&r), "{r:?}");
    assert_all_installed(&env);
    assert!(claude_log(&env).is_empty(), "{:?}", claude_log(&env));
    assert_eq!(read_json(&settings_path)["hooks"], hook_settings());
    assert_eq!(read_json(&state_path)["items"], state_before);
    assert!(env
        .home
        .join(format!(".claude/triple-c/plugins/{SLUG_A}"))
        .is_dir());

    // A real deselection afterwards still removes everything.
    payload(&env, &[], empty_manifest());
    let r = run(&env);
    assert_eq!(r.removed.len(), 5, "{r:?}");
}

#[test]
fn a_malformed_held_list_changes_nothing() {
    let Some(env) = env() else { return };
    install_all(&env);
    for bad in [json!("agent:code-reviewer"), json!([1]), json!({})] {
        let mut manifest = empty_manifest();
        manifest["held"] = bad.clone();
        payload(&env, &[], manifest);
        let r = run(&env);
        assert!(r.removed.is_empty(), "{bad}: {r:?}");
        assert_eq!(r.errors.len(), 1, "{bad}: {r:?}");
        assert_all_installed(&env);
    }
}
