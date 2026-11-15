use std::collections::BTreeMap;
use std::io::Write;
use std::time::Duration;

use super::*;
use crate::aws::tofu_modules::{boundary_vars, module_files, Deployment, ModuleKind};
use crate::models::host_connection::{AwsAuth, HostConnection, HostKind};
use crate::storage::secure::AwsKeys;

fn profile_conn(name: &str) -> HostConnection {
    HostConnection {
        id: "c1".into(),
        name: "Test".into(),
        kind: HostKind::Aws {
            auth: AwsAuth::Profile { name: name.into() },
            default_region: "us-east-1".into(),
        },
    }
}

fn keys_conn() -> HostConnection {
    HostConnection {
        id: "c2".into(),
        name: "Keys".into(),
        kind: HostKind::Aws {
            auth: AwsAuth::Keys {
                keyring_ref: "c2".into(),
            },
            default_region: "us-east-1".into(),
        },
    }
}

fn parent_env() -> Vec<(String, String)> {
    [
        ("PATH", "/usr/bin:/bin"),
        ("HOME", "/home/u"),
        ("AWS_ENDPOINT_URL", "http://evil.example"),
        ("AWS_ACCESS_KEY_ID", "AKIAPARENTNOTREAL"),
        ("AWS_PROFILE", "parent-profile"),
        ("TF_LOG", "TRACE"),
        ("TF_PLUGIN_CACHE_DIR", "/parent/cache"),
        ("TOFU_X", "1"),
        ("CHECKPOINT_DISABLE", "0"),
        ("GITHUB_TOKEN", "parent-token-not-real"),
        ("LD_PRELOAD", "/tmp/evil.so"),
        ("HTTPS_PROXY", "http://proxy.corp:3128"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

#[test]
fn a_download_with_the_wrong_hash_is_deleted_and_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("tofu_fake.tar.gz");
    std::fs::write(&archive, b"not really an archive").unwrap();
    let dest = tmp.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();
    let wrong = "0".repeat(64);

    let result = verify_and_extract(&archive, &wrong, &dest);

    assert!(result.is_err());
    assert!(!archive.exists(), "a mismatched archive must be deleted");
    assert_eq!(
        std::fs::read_dir(&dest).unwrap().count(),
        0,
        "nothing extracted"
    );
}

#[test]
fn a_verified_archive_yields_only_the_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("tofu.tar.gz");
    {
        let gz = flate2::write::GzEncoder::new(
            std::fs::File::create(&archive).unwrap(),
            flate2::Compression::fast(),
        );
        let mut tar = tar::Builder::new(gz);
        for (name, body) in [("LICENSE", &b"mpl"[..]), (TOFU_BIN, &b"#!fake"[..])] {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, name, body).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
    }
    let sha = sha256_file(&archive).unwrap();
    let dest = tmp.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    let bin = verify_and_extract(&archive, &sha, &dest).unwrap();

    assert_eq!(bin, dest.join(TOFU_BIN));
    assert_eq!(std::fs::read(&bin).unwrap(), b"#!fake");
    assert!(!dest.join("LICENSE").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&bin).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }
}

fn env_map(v: Vec<(String, String)>) -> BTreeMap<String, String> {
    v.into_iter().collect()
}

fn get<'a>(env: &'a BTreeMap<String, String>, k: &str) -> Option<&'a str> {
    env.get(k).map(String::as_str)
}

#[test]
fn the_child_environment_carries_nothing_inherited() {
    let root = std::path::Path::new("/app");

    let env = env_map(child_env(&profile_conn("work"), None, root, parent_env()).unwrap());

    for gone in [
        "AWS_ENDPOINT_URL",
        "AWS_ACCESS_KEY_ID",
        "TF_LOG",
        "TOFU_X",
        "GITHUB_TOKEN",
        "LD_PRELOAD",
    ] {
        assert!(!env.contains_key(gone), "{gone} leaked: {env:?}");
    }
    assert_eq!(get(&env, "AWS_PROFILE"), Some("work"));
    assert_eq!(
        get(&env, "TF_PLUGIN_CACHE_DIR"),
        Some("/app/tofu/plugin-cache")
    );
    assert_eq!(get(&env, "TF_CLI_CONFIG_FILE"), Some("/app/tofu/tofurc"));
    assert_eq!(get(&env, "TF_IN_AUTOMATION"), Some("1"));
    assert_eq!(get(&env, "TF_INPUT"), Some("0"));
    assert_eq!(get(&env, "CHECKPOINT_DISABLE"), Some("1"));
    assert_eq!(
        get(&env, "AWS_IGNORE_CONFIGURED_ENDPOINT_URLS"),
        Some("true")
    );
    assert_eq!(get(&env, "PATH"), Some("/usr/bin:/bin"));
    assert_eq!(get(&env, "HOME"), Some("/home/u"));
    assert_eq!(get(&env, "HTTPS_PROXY"), Some("http://proxy.corp:3128"));
    let aws: Vec<_> = env.keys().filter(|k| k.starts_with("AWS_")).collect();
    assert_eq!(aws, ["AWS_IGNORE_CONFIGURED_ENDPOINT_URLS", "AWS_PROFILE"]);
    let tf: Vec<_> = env
        .keys()
        .filter(|k| k.starts_with("TF_") || k.starts_with("TOFU_"))
        .collect();
    assert_eq!(
        tf,
        [
            "TF_CLI_CONFIG_FILE",
            "TF_INPUT",
            "TF_IN_AUTOMATION",
            "TF_PLUGIN_CACHE_DIR"
        ]
    );

    let keys = AwsKeys {
        access_key_id: "AKIACONNNOTREAL".into(),
        secret_access_key: "conn-secret-not-real".into(),
        session_token: Some("conn-token-not-real".into()),
    };
    let env = env_map(child_env(&keys_conn(), Some(&keys), root, parent_env()).unwrap());
    assert_eq!(get(&env, "AWS_ACCESS_KEY_ID"), Some("AKIACONNNOTREAL"));
    assert_eq!(
        get(&env, "AWS_SECRET_ACCESS_KEY"),
        Some("conn-secret-not-real")
    );
    assert_eq!(get(&env, "AWS_SESSION_TOKEN"), Some("conn-token-not-real"));
    // The user's own AWS config can't mix into a keys connection.
    assert_eq!(
        get(&env, "AWS_CONFIG_FILE"),
        Some("/app/tofu/no-aws-config")
    );
    assert_eq!(
        get(&env, "AWS_SHARED_CREDENTIALS_FILE"),
        Some("/app/tofu/no-aws-config")
    );
    let aws: Vec<_> = env.keys().filter(|k| k.starts_with("AWS_")).collect();
    assert_eq!(
        aws,
        [
            "AWS_ACCESS_KEY_ID",
            "AWS_CONFIG_FILE",
            "AWS_IGNORE_CONFIGURED_ENDPOINT_URLS",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_SHARED_CREDENTIALS_FILE",
        ]
    );
}

#[test]
fn a_keys_connection_without_keys_is_refused() {
    let result = child_env(
        &keys_conn(),
        None,
        std::path::Path::new("/app"),
        parent_env(),
    );
    assert!(
        matches!(result, Err(crate::aws::AwsError::KeysMissing)),
        "{result:?}"
    );
}

#[test]
fn profile_files_and_the_ca_bundle_follow_the_app() {
    let mut parent = parent_env();
    parent.push(("AWS_CONFIG_FILE".into(), "~/work/aws-config".into()));
    parent.push((
        "AWS_SHARED_CREDENTIALS_FILE".into(),
        "/etc/aws/creds".into(),
    ));
    parent.push(("AWS_CA_BUNDLE".into(), "/etc/corp-ca.pem".into()));
    let root = std::path::Path::new("/app");

    let env = env_map(child_env(&profile_conn("work"), None, root, parent.clone()).unwrap());
    assert_eq!(
        get(&env, "AWS_CONFIG_FILE"),
        Some("/home/u/work/aws-config")
    );
    assert_eq!(
        get(&env, "AWS_SHARED_CREDENTIALS_FILE"),
        Some("/etc/aws/creds")
    );
    assert_eq!(get(&env, "AWS_CA_BUNDLE"), Some("/etc/corp-ca.pem"));

    let keys = AwsKeys {
        access_key_id: "AKIACONNNOTREAL".into(),
        secret_access_key: "conn-secret-not-real".into(),
        session_token: None,
    };
    let env = env_map(child_env(&keys_conn(), Some(&keys), root, parent).unwrap());
    assert_eq!(
        get(&env, "AWS_CONFIG_FILE"),
        Some("/app/tofu/no-aws-config")
    );
    assert_eq!(get(&env, "AWS_CA_BUNDLE"), Some("/etc/corp-ca.pem"));
    assert!(!env.contains_key("AWS_SESSION_TOKEN"));
}

#[tokio::test]
async fn ensure_prepares_the_cli_config_and_keeps_the_no_config_path_absent() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("tofu/no-aws-config")).unwrap();
    // Not reaching the download: the support files come first, and a failed
    // download is fine for this test.
    let _ = prepare_support_files(root.path());
    assert_eq!(
        std::fs::read_to_string(root.path().join("tofu/tofurc")).unwrap(),
        CLI_CONFIG
    );
    assert!(root.path().join("tofu/plugin-cache").is_dir());
    assert!(!root.path().join("tofu/no-aws-config").exists());
}

#[test]
fn the_runner_hands_tofu_exactly_the_apps_variables_without_appimage_changes() {
    let appdir = "/tmp/.mount_triplecAbc";
    let own: BTreeMap<String, String> = [
        (ENV_KEYS_VAR, "PATH,HOME,AWS_PROFILE,TF_IN_AUTOMATION"),
        (TOFU_BIN_ENV, "/app/tofu/1.13.9/tofu"),
        (
            "PATH",
            "/tmp/.mount_triplecAbc/usr/bin:/usr/local/bin:/usr/bin",
        ),
        ("HOME", "/home/u"),
        ("AWS_PROFILE", "work"),
        ("TF_IN_AUTOMATION", "1"),
        // What AppRun and the GTK hook add:
        ("APPDIR", appdir),
        ("APPIMAGE", "/home/u/Triple-C.AppImage"),
        ("ARGV0", "Triple-C.AppImage"),
        ("OWD", "/home/u"),
        ("LD_LIBRARY_PATH", "/tmp/.mount_triplecAbc/usr/lib"),
        ("PYTHONPATH", "/tmp/.mount_triplecAbc/usr/share/pyshared"),
        (
            "XDG_DATA_DIRS",
            "/tmp/.mount_triplecAbc/usr/share:/usr/share",
        ),
        ("GTK_PATH", "/tmp/.mount_triplecAbc/usr/lib/gtk-3.0"),
        (
            "GDK_PIXBUF_MODULE_FILE",
            "/tmp/.mount_triplecAbc/usr/lib/loaders.cache",
        ),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();

    let env = env_map(tofu_env(&own));

    let expected: BTreeMap<String, String> = [
        ("PATH", "/usr/local/bin:/usr/bin"),
        ("HOME", "/home/u"),
        ("AWS_PROFILE", "work"),
        ("TF_IN_AUTOMATION", "1"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert_eq!(env, expected);

    // A launcher that saved the original gets it restored.
    let mut saved = own.clone();
    saved.insert("PATH_ORIG".into(), "/usr/bin:/bin".into());
    assert_eq!(
        get(&env_map(tofu_env(&saved)), "PATH"),
        Some("/usr/bin:/bin")
    );

    // Without the key list, nothing at all.
    let mut bare = own;
    bare.remove(ENV_KEYS_VAR);
    assert!(tofu_env(&bare).is_empty());
}

#[test]
fn materialize_writes_backend_and_vars_privately() {
    let tmp = tempfile::tempdir().unwrap();
    let vars = boundary_vars("us-east-1");

    let wd = materialize(
        tmp.path(),
        ModuleKind::PermissionsBoundary,
        Deployment::Boundary,
        "triple-c-tfstate-123456789012-us-east-1",
        "us-east-1",
        &vars,
    )
    .unwrap();

    assert_eq!(
        wd.dir,
        tmp.path()
            .join("work/triple-c-tfstate-123456789012-us-east-1/boundary/terraform")
    );
    let backend = std::fs::read_to_string(wd.dir.join("backend.hcl")).unwrap();
    assert!(
        backend.contains(r#"bucket = "triple-c-tfstate-123456789012-us-east-1""#),
        "{backend}"
    );
    assert!(
        backend.contains(r#"key = "boundary/terraform.tfstate""#),
        "{backend}"
    );
    assert!(backend.contains(r#"region = "us-east-1""#), "{backend}");
    assert!(backend.contains("use_lockfile = true"), "{backend}");
    assert!(backend.contains("encrypt = true"), "{backend}");

    let tfvars = wd.dir.join("terraform.tfvars.json");
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&tfvars).unwrap()).unwrap();
    assert_eq!(written, vars);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&tfvars).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    for (name, text) in module_files(ModuleKind::PermissionsBoundary) {
        assert_eq!(
            std::fs::read(wd.dir.join(name)).unwrap(),
            text.as_bytes(),
            "{name}"
        );
    }
}

#[test]
fn materialize_refuses_names_that_leave_the_work_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let vars = serde_json::json!({});
    for (bucket, region) in [
        ("../x", "us-east-1"),
        ("b", "us-east-1\"\n"),
        ("", "us-east-1"),
    ] {
        assert!(materialize(
            tmp.path(),
            ModuleKind::PermissionsBoundary,
            Deployment::Boundary,
            bucket,
            region,
            &vars
        )
        .is_err());
    }
    assert!(materialize(
        tmp.path(),
        ModuleKind::Environment,
        Deployment::Environment(".."),
        "b",
        "us-east-1",
        &vars
    )
    .is_err());
}

#[test]
fn workdir_for_names_the_materialized_folder_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let bucket = "triple-c-tfstate-123456789012-us-east-1";
    let user = Deployment::User {
        env: "dev",
        owner_key: "0123456789ab",
    };

    let wd = workdir_for(tmp.path(), user, bucket).unwrap();
    assert_eq!(
        wd.dir,
        tmp.path()
            .join(format!("work/{bucket}/users/dev/0123456789ab/terraform"))
    );
    assert!(
        !tmp.path().join("work").exists(),
        "workdir_for wrote something"
    );
    assert!(!is_running(&wd));

    let made = materialize(
        tmp.path(),
        ModuleKind::UserOnboard,
        user,
        bucket,
        "us-east-1",
        &serde_json::json!({}),
    )
    .unwrap();
    assert_eq!(made, wd);

    assert!(workdir_for(tmp.path(), Deployment::Environment(".."), bucket).is_err());
    assert!(workdir_for(tmp.path(), Deployment::Boundary, "../x").is_err());
}

fn append(path: &std::path::Path, lines: &[String]) {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

fn json_line(i: usize) -> String {
    format!(
        r#"{{"@level":"info","@message":"event {i}","@module":"tofu.ui","@timestamp":"2026-11-14T00:00:0{i}Z","type":"apply_progress"}}"#
    )
}

#[tokio::test]
async fn following_an_existing_log_reports_each_event_once() {
    let tmp = tempfile::tempdir().unwrap();
    let wd = Workdir {
        dir: tmp.path().to_path_buf(),
    };
    let log = wd.dir.join("apply.log");
    append(&log, &(0..3).map(json_line).collect::<Vec<_>>());
    // Stand in for the runner: it holds the lock until it has written the
    // exit code.
    let lock = hold_lock(&wd);

    let dir = wd.dir.clone();
    let writer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(60)).await;
        append(
            &dir.join("apply.log"),
            &(3..5).map(json_line).collect::<Vec<_>>(),
        );
        tokio::time::sleep(Duration::from_millis(60)).await;
        std::fs::write(dir.join("apply.exit"), "0").unwrap();
        drop(lock);
    });

    let mut seen = Vec::new();
    let outcome = follow(&wd, Duration::from_millis(10), |events| {
        seen.extend(events.iter().map(|e| e.message.clone()))
    })
    .await
    .unwrap();
    writer.await.unwrap();

    assert_eq!(
        seen,
        (0..5).map(|i| format!("event {i}")).collect::<Vec<_>>()
    );
    assert!(outcome.ok);
    assert_eq!(outcome.exit_code, 0);
}

#[tokio::test]
async fn a_failed_run_reports_its_exit_code_and_last_error() {
    let tmp = tempfile::tempdir().unwrap();
    let wd = Workdir {
        dir: tmp.path().to_path_buf(),
    };
    append(&wd.dir.join("apply.log"), &[DIAGNOSTIC.to_string()]);
    std::fs::write(wd.dir.join("apply.exit"), "1\n").unwrap();

    let outcome = follow(&wd, Duration::from_millis(10), |_| {})
        .await
        .unwrap();

    assert!(!outcome.ok);
    assert_eq!(outcome.exit_code, 1);
    assert!(outcome
        .summary
        .unwrap()
        .contains("local-exec provisioner error"));
}

/// Real `tofu apply -json` output (OpenTofu 1.13.9, a failing `local-exec`).
const APPLY_ERRORED: &str = r#"{"@level":"info","@message":"terraform_data.boom: Creation errored after 0s","@module":"tofu.ui","@timestamp":"2026-11-14T21:39:42.012032-08:00","hook":{"resource":{"addr":"terraform_data.boom","module":"","resource":"terraform_data.boom","implied_provider":"terraform","resource_type":"terraform_data","resource_name":"boom","resource_key":null},"action":"create","elapsed_seconds":0},"type":"apply_errored"}"#;
const DIAGNOSTIC: &str = r#"{"@level":"error","@message":"Error: local-exec provisioner error","@module":"tofu.ui","@timestamp":"2026-11-14T21:39:42.020787-08:00","diagnostic":{"severity":"error","summary":"local-exec provisioner error","detail":"Error running command 'false': exit status 1. Output: ","address":"terraform_data.boom","range":{"filename":"main.tf","start":{"line":2,"column":28,"byte":62},"end":{"line":2,"column":29,"byte":63}},"snippet":{"context":"resource \"terraform_data\" \"boom\"","code":"  provisioner \"local-exec\" {","start_line":2,"highlight_start_offset":27,"highlight_end_offset":28,"values":[]}},"type":"diagnostic"}"#;

#[test]
fn apply_errored_and_diagnostics_surface_their_message() {
    let e = parse_log_line(APPLY_ERRORED).unwrap();
    assert_eq!(e.kind, "apply_errored");
    assert_eq!(e.level, "error");
    assert_eq!(e.resource.as_deref(), Some("terraform_data.boom"));
    assert_eq!(e.action.as_deref(), Some("create"));
    assert_eq!(e.message, "terraform_data.boom: Creation errored after 0s");
    assert_eq!(e.ts, "2026-11-14T21:39:42.012032-08:00");

    let d = parse_log_line(DIAGNOSTIC).unwrap();
    assert_eq!(d.kind, "diagnostic");
    assert_eq!(d.level, "error");
    assert_eq!(d.resource.as_deref(), Some("terraform_data.boom"));
    assert!(
        d.message.contains("local-exec provisioner error"),
        "{}",
        d.message
    );
    assert!(d.message.contains("exit status 1"), "{}", d.message);

    let plain = parse_log_line("Initializing the backend...").unwrap();
    assert_eq!(plain.kind, "log");
    assert_eq!(plain.message, "Initializing the backend...");
    assert!(parse_log_line("   ").is_none());
}

#[test]
fn release_assets_exist_for_every_shipped_platform() {
    for (os, arch) in [
        ("linux", "x86_64"),
        ("linux", "aarch64"),
        ("macos", "x86_64"),
        ("macos", "aarch64"),
        ("windows", "x86_64"),
    ] {
        let (file, sha) = release_asset(os, arch).unwrap_or_else(|| panic!("{os}/{arch}"));
        assert!(file.starts_with(&format!("tofu_{TOFU_VERSION}_")), "{file}");
        assert!(file.ends_with(".tar.gz"), "{file}");
        assert_eq!(sha.len(), 64, "{file}");
        assert!(
            sha.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "{file}"
        );
    }
    assert_eq!(TOFU_VERSION, "1.13.9");
    assert!(release_asset(std::env::consts::OS, std::env::consts::ARCH).is_some());
    assert!(release_asset("freebsd", "x86_64").is_none());
}

fn hold_lock(wd: &Workdir) -> std::fs::File {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(wd.dir.join("apply.lock"))
        .unwrap();
    f.try_lock().unwrap();
    f
}

#[test]
fn a_live_but_unrelated_pid_is_not_a_running_deploy() {
    let tmp = tempfile::tempdir().unwrap();
    let wd = Workdir {
        dir: tmp.path().to_path_buf(),
    };
    assert!(!is_running(&wd));
    // A PID that is certainly alive (this test process), and no lock: a
    // reused PID after a reboot looks exactly like this.
    std::fs::write(wd.dir.join("apply.pid"), std::process::id().to_string()).unwrap();
    assert!(!is_running(&wd), "a live PID alone is not a run");
    assert!(
        interrupt(&wd).is_err(),
        "nothing is signalled without the lock"
    );
    std::fs::write(wd.dir.join("apply.lock"), "").unwrap();
    assert!(!is_running(&wd), "an unheld lock file is not a run");

    let lock = hold_lock(&wd);
    assert!(is_running(&wd), "the lock is held");
    std::fs::write(wd.dir.join("apply.exit"), "0").unwrap();
    assert!(!is_running(&wd), "an exit code means the run is over");
    drop(lock);
}

#[tokio::test]
async fn a_runner_that_died_without_an_exit_code_ends_the_follow() {
    let tmp = tempfile::tempdir().unwrap();
    let wd = Workdir {
        dir: tmp.path().to_path_buf(),
    };
    append(&wd.dir.join("apply.log"), &[json_line(0)]);
    std::fs::write(wd.dir.join("apply.pid"), std::process::id().to_string()).unwrap();

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        follow(&wd, Duration::from_millis(10), |_| {}),
    )
    .await
    .expect("follow must not wait forever")
    .unwrap();
    assert!(!outcome.ok);
    assert_eq!(outcome.exit_code, -1);
}

/// Real `tofu init -json` output for a bad `backend.hcl`.
const INIT_DIAGNOSTIC: &str = r#"{"@level":"error","@message":"Error: Unsupported argument","@module":"tofu.ui","@timestamp":"2026-11-14T21:53:38.405933-08:00","diagnostic":{"severity":"error","summary":"Unsupported argument","detail":"An argument named \"nonsense_attr\" is not expected here.","range":{"filename":"backend.hcl","start":{"line":1,"column":1,"byte":0},"end":{"line":1,"column":14,"byte":13}},"snippet":{"context":null,"code":"nonsense_attr = 1","start_line":1,"highlight_start_offset":0,"highlight_end_offset":13,"values":[]}},"type":"diagnostic"}"#;

#[cfg(unix)]
fn fake_tofu(dir: &std::path::Path, script: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-tofu");
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// The runner's state lives in statics; the in-process runner tests take
/// turns.
#[cfg(unix)]
static RUNNER_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A turn at the runner statics. On release it puts SIGINT back to its
/// default, so the forwarding handler `run_runner` installs doesn't make
/// this test process ignore Ctrl-C.
#[cfg(unix)]
struct RunnerTurn(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

#[cfg(unix)]
impl Drop for RunnerTurn {
    fn drop(&mut self) {
        // SAFETY: restores the default disposition.
        unsafe {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
        }
    }
}

#[cfg(unix)]
fn runner_turn() -> RunnerTurn {
    RunnerTurn(RUNNER_TESTS.lock().unwrap_or_else(|e| e.into_inner()))
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_init_still_writes_the_exit_code_and_a_summary() {
    let _turn = runner_turn();
    let bin = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let wd = Workdir {
        dir: tmp.path().to_path_buf(),
    };
    let tofu = fake_tofu(
        bin.path(),
        &format!("[ \"$1\" = init ] || {{ echo apply-ran; exit 0; }}\ncat <<'EOF'\n{INIT_DIAGNOSTIC}\nEOF\nexit 1"),
    );
    let env = vec![("PATH".to_string(), "/usr/bin:/bin".to_string())];

    let code = run_runner(&wd.dir, TofuOp::Apply, &tofu, &env);

    assert_eq!(code, Some(1));
    assert_eq!(
        std::fs::read_to_string(wd.dir.join("apply.exit"))
            .unwrap()
            .trim(),
        "1"
    );
    assert!(!std::fs::read_to_string(wd.dir.join("apply.log"))
        .unwrap()
        .contains("apply-ran"));
    assert!(!is_running(&wd), "the lock goes with the runner");
    let outcome = follow(&wd, Duration::from_millis(10), |_| {})
        .await
        .unwrap();
    assert!(!outcome.ok);
    let summary = outcome.summary.unwrap();
    assert!(summary.contains("Unsupported argument"), "{summary}");
}

#[cfg(unix)]
#[test]
fn the_runner_gives_tofu_only_its_environment() {
    let _turn = runner_turn();
    let bin = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let tofu = fake_tofu(bin.path(), "env | sort");
    let env = vec![
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ("AWS_PROFILE".to_string(), "work".to_string()),
    ];

    assert_eq!(run_runner(tmp.path(), TofuOp::Apply, &tofu, &env), Some(0));

    let log = std::fs::read_to_string(tmp.path().join("apply.log")).unwrap();
    assert!(log.contains("AWS_PROFILE=work"), "{log}");
    for line in log
        .lines()
        .filter(|l| l.contains('=') && !l.starts_with("triple-c:"))
    {
        let name = line.split('=').next().unwrap();
        // `sh` itself adds PWD/SHLVL/_; nothing else may appear.
        assert!(
            ["PATH", "AWS_PROFILE", "PWD", "SHLVL", "_"].contains(&name),
            "unexpected variable {name} in tofu's environment"
        );
    }
}

/// Cancel signals the runner (here: this process) alone; the runner's
/// handler forwards SIGINT to `tofu`, which stops; the runner records it.
#[cfg(unix)]
#[test]
fn cancel_reaches_tofu_through_the_runner_and_the_exit_is_recorded() {
    let _turn = runner_turn();
    let bin = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let wd = Workdir {
        dir: tmp.path().to_path_buf(),
    };
    let tofu = fake_tofu(
        bin.path(),
        "[ \"$1\" = init ] && exit 0\necho started\nexec sleep 30",
    );
    let env = vec![("PATH".to_string(), "/usr/bin:/bin".to_string())];

    let dir = wd.dir.clone();
    let runner = std::thread::spawn(move || run_runner(&dir, TofuOp::Apply, &tofu, &env));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !(CHILD_PID.load(Ordering::SeqCst) > 0
        && std::fs::read_to_string(wd.dir.join("apply.log"))
            .unwrap_or_default()
            .contains("started"))
    {
        assert!(std::time::Instant::now() < deadline, "apply never started");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(is_running(&wd));
    assert_eq!(read_pid(&wd), Some(std::process::id()));

    let started = std::time::Instant::now();
    interrupt(&wd).unwrap();
    let code = runner.join().unwrap();

    assert!(
        started.elapsed() < Duration::from_secs(10),
        "tofu was not interrupted"
    );
    assert_eq!(code, Some(128 + libc::SIGINT));
    assert_eq!(
        std::fs::read_to_string(wd.dir.join("apply.exit"))
            .unwrap()
            .trim(),
        (128 + libc::SIGINT).to_string()
    );
    assert!(!is_running(&wd));
}

/// Downloads OpenTofu (network; no AWS): `cargo test aws::tofu::tests::smoke
/// -- --ignored --nocapture`. Fetches the pinned release into a temp root,
/// reuses it on a second call, materializes the boundary module and runs
/// `init -backend=false` + `validate` through that binary.
#[tokio::test]
#[ignore]
async fn smoke_the_downloaded_tofu_validates_the_boundary_module() {
    let root = tempfile::tempdir().unwrap();
    let tofu = ensure_tofu(root.path()).await.unwrap();
    assert_eq!(ensure_tofu(root.path()).await.unwrap(), tofu, "reused");
    let wd = materialize(
        root.path(),
        ModuleKind::PermissionsBoundary,
        Deployment::Boundary,
        "triple-c-tfstate-000000000000-us-east-1",
        "us-east-1",
        &boundary_vars("us-east-1"),
    )
    .unwrap();
    let env = child_env(&profile_conn("none"), None, root.path(), std::env::vars()).unwrap();
    for args in [
        &["version"][..],
        &["init", "-backend=false", "-input=false", "-no-color"],
        &["validate", "-no-color"],
    ] {
        let out = std::process::Command::new(&tofu)
            .args(args)
            .current_dir(&wd.dir)
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k, v)))
            .output()
            .unwrap();
        println!(
            "$ tofu {}\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "tofu {args:?} failed");
    }
}

#[test]
fn a_second_start_while_the_runner_holds_the_lock_touches_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let wd = Workdir {
        dir: tmp.path().to_path_buf(),
    };
    let lock = hold_lock(&wd);
    std::fs::write(wd.dir.join("apply.pid"), "4242").unwrap();
    std::fs::write(wd.dir.join("apply.log"), "first run\n").unwrap();

    let result = start_detached(
        std::path::Path::new("/nonexistent/tofu"),
        &wd,
        TofuOp::Destroy,
        &[],
    );

    assert!(
        matches!(result, Err(StartError::AlreadyRunning)),
        "{result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(wd.dir.join("apply.pid")).unwrap(),
        "4242"
    );
    assert_eq!(
        std::fs::read_to_string(wd.dir.join("apply.log")).unwrap(),
        "first run\n"
    );
    drop(lock);
}

#[test]
fn a_start_already_under_way_makes_a_second_one_fail_fast() {
    let tmp = tempfile::tempdir().unwrap();
    let wd = Workdir {
        dir: tmp.path().to_path_buf(),
    };
    let starting = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(wd.dir.join("start.lock"))
        .unwrap();
    starting.try_lock().unwrap();
    std::fs::write(wd.dir.join("apply.pid"), "4242").unwrap();

    let result = start_detached(
        std::path::Path::new("/nonexistent/tofu"),
        &wd,
        TofuOp::Apply,
        &[],
    );

    assert!(
        matches!(result, Err(StartError::AlreadyRunning)),
        "{result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(wd.dir.join("apply.pid")).unwrap(),
        "4242"
    );
}

#[cfg(target_os = "linux")]
fn fake_runner(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    // Invoked as: <program> --tofu-runner <workdir> <op>
    let path = dir.join("fake-runner");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A runner that took the lock but never wrote its PID is running a deploy:
/// the start returns (Cancel unavailable) and does not kill it.
#[cfg(target_os = "linux")]
#[test]
fn a_runner_holding_the_lock_without_a_pid_is_never_killed() {
    if !std::path::Path::new("/usr/bin/flock").exists() {
        eprintln!("skipped: no flock(1)");
        return;
    }
    let bin = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let wd = Workdir {
        dir: tmp.path().to_path_buf(),
    };
    let program = fake_runner(bin.path(), "exec /usr/bin/flock \"$2/apply.lock\" sleep 6");
    let env = vec![("PATH".to_string(), "/usr/bin:/bin".to_string())];

    let started = start_detached_with(
        &program,
        Duration::from_secs(10),
        std::path::Path::new("/unused"),
        &wd,
        TofuOp::Apply,
        &env,
    )
    .unwrap();

    assert_eq!(started, Started { pid: None });
    assert!(is_running(&wd), "the runner must still be alive");
    // And the start lock was released for the next start, which is refused
    // because a runner holds the deployment.
    let again = start_detached(std::path::Path::new("/unused"), &wd, TofuOp::Apply, &[]);
    assert!(
        matches!(again, Err(StartError::AlreadyRunning)),
        "{again:?}"
    );
}

/// Only a runner that never took the lock and never finished is stopped.
#[cfg(target_os = "linux")]
#[test]
fn a_runner_that_never_takes_the_lock_is_stopped_and_reported() {
    let bin = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let wd = Workdir {
        dir: tmp.path().to_path_buf(),
    };
    let program = fake_runner(bin.path(), "exec sleep 30");
    let env = vec![("PATH".to_string(), "/usr/bin:/bin".to_string())];

    let started = std::time::Instant::now();
    let result = start_detached_with(
        &program,
        Duration::from_millis(300),
        std::path::Path::new("/unused"),
        &wd,
        TofuOp::Apply,
        &env,
    );

    assert!(
        matches!(&result, Err(StartError::Failed(e)) if e.to_string().contains("did not start in time")),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// A runner that takes the lock and writes its PID is reported with it.
#[cfg(target_os = "linux")]
#[test]
fn a_started_runner_reports_its_pid() {
    if !std::path::Path::new("/usr/bin/flock").exists() {
        eprintln!("skipped: no flock(1)");
        return;
    }
    let bin = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let wd = Workdir {
        dir: tmp.path().to_path_buf(),
    };
    let program = fake_runner(
        bin.path(),
        "exec /usr/bin/flock \"$2/apply.lock\" sh -c 'echo 31337 > \"$0/apply.pid\"; sleep 3' \"$2\"",
    );
    let env = vec![("PATH".to_string(), "/usr/bin:/bin".to_string())];

    let started = start_detached_with(
        &program,
        Duration::from_secs(10),
        std::path::Path::new("/unused"),
        &wd,
        TofuOp::Apply,
        &env,
    )
    .unwrap();

    assert_eq!(started, Started { pid: Some(31337) });
}

/// A failed apply whose state OpenTofu could not save leaves
/// `errored.tfstate`: the runner says so in an error line, the outcome's
/// summary names it, and nothing new is materialized over it.
#[cfg(unix)]
#[tokio::test]
async fn an_errored_state_is_reported_and_blocks_the_next_launch() {
    let _turn = runner_turn();
    let bin = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let bucket = "triple-c-tfstate-123456789012-us-east-1";
    let wd = materialize(
        root.path(),
        ModuleKind::PermissionsBoundary,
        Deployment::Boundary,
        bucket,
        "us-east-1",
        &boundary_vars("us-east-1"),
    )
    .unwrap();
    // init succeeds; the apply "fails to persist" and leaves errored.tfstate.
    let tofu = fake_tofu(
        bin.path(),
        "[ \"$1\" = init ] && exit 0\necho '{\"version\":4}' > errored.tfstate\nexit 1",
    );
    let env = vec![("PATH".to_string(), "/usr/bin:/bin".to_string())];

    assert_eq!(run_runner(&wd.dir, TofuOp::Apply, &tofu, &env), Some(1));
    assert!(has_errored_state(&wd));
    assert!(last_run_failed(&wd));
    let log = std::fs::read_to_string(wd.dir.join("apply.log")).unwrap();
    assert!(
        log.lines()
            .any(|l| l.starts_with("Error:") && l.contains("errored.tfstate")),
        "{log}"
    );
    let outcome = follow(&wd, Duration::from_millis(10), |_| {})
        .await
        .unwrap();
    assert!(!outcome.ok);
    let summary = outcome.summary.unwrap();
    assert!(summary.contains("errored.tfstate"), "{summary}");
    assert!(summary.contains("Push saved state"), "{summary}");

    // Launching anything there now is refused, naming the recovery.
    let err = materialize(
        root.path(),
        ModuleKind::PermissionsBoundary,
        Deployment::Boundary,
        bucket,
        "us-east-1",
        &boundary_vars("us-east-1"),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("errored.tfstate"), "{err}");
    assert!(err.contains("tofu state push errored.tfstate"), "{err}");

    // The recovery: push it back through the runner; it is set aside once
    // pushed, and the folder can be used again.
    let pushed = fake_tofu(
        bin.path(),
        "[ \"$1\" = init ] && exit 0\n[ \"$1 $2 $3\" = 'state push errored.tfstate' ] || exit 9\necho pushed",
    );
    assert_eq!(run_runner(&wd.dir, TofuOp::PushState, &pushed, &env), Some(0));
    assert!(!has_errored_state(&wd));
    assert!(wd.dir.join("errored.tfstate.pushed").exists());
    assert!(!last_run_failed(&wd));
    assert!(materialize(
        root.path(),
        ModuleKind::PermissionsBoundary,
        Deployment::Boundary,
        bucket,
        "us-east-1",
        &boundary_vars("us-east-1"),
    )
    .is_ok());
}

#[cfg(unix)]
#[test]
fn a_failed_push_keeps_the_saved_state() {
    let _turn = runner_turn();
    let bin = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let wd = Workdir {
        dir: tmp.path().to_path_buf(),
    };
    std::fs::write(wd.dir.join("errored.tfstate"), "{}").unwrap();
    let tofu = fake_tofu(bin.path(), "[ \"$1\" = init ] && exit 0\nexit 1");
    let env = vec![("PATH".to_string(), "/usr/bin:/bin".to_string())];
    assert_eq!(run_runner(&wd.dir, TofuOp::PushState, &tofu, &env), Some(1));
    assert!(has_errored_state(&wd));
}

/// materialize checks for a running deploy under `start.lock`, so a second
/// app instance between its check and its start can't interleave.
#[test]
fn materialize_waits_its_turn_on_the_start_lock() {
    let root = tempfile::tempdir().unwrap();
    let bucket = "triple-c-tfstate-123456789012-us-east-1";
    let wd = workdir_for(root.path(), Deployment::Boundary, bucket).unwrap();
    std::fs::create_dir_all(&wd.dir).unwrap();
    let starting = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(wd.dir.join("start.lock"))
        .unwrap();
    starting.try_lock().unwrap();
    let err = materialize(
        root.path(),
        ModuleKind::PermissionsBoundary,
        Deployment::Boundary,
        bucket,
        "us-east-1",
        &boundary_vars("us-east-1"),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("already"), "{err}");
    assert!(!wd.dir.join("backend.hcl").exists(), "materialize wrote under another's lock");
    drop(starting);
}
