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

#[test]
fn the_child_environment_carries_nothing_inherited() {
    let cache = std::path::Path::new("/app/plugin-cache");

    let env: BTreeMap<String, String> = child_env(&profile_conn("work"), None, cache, parent_env())
        .into_iter()
        .collect();

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
    assert_eq!(env.get("AWS_PROFILE").map(String::as_str), Some("work"));
    assert_eq!(
        env.get("TF_PLUGIN_CACHE_DIR").map(String::as_str),
        Some("/app/plugin-cache")
    );
    assert_eq!(env.get("TF_IN_AUTOMATION").map(String::as_str), Some("1"));
    assert_eq!(env.get("TF_INPUT").map(String::as_str), Some("0"));
    assert_eq!(env.get("CHECKPOINT_DISABLE").map(String::as_str), Some("1"));
    assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin:/bin"));
    assert_eq!(env.get("HOME").map(String::as_str), Some("/home/u"));
    assert_eq!(
        env.get("HTTPS_PROXY").map(String::as_str),
        Some("http://proxy.corp:3128")
    );
    let aws: Vec<_> = env.keys().filter(|k| k.starts_with("AWS_")).collect();
    assert_eq!(aws, ["AWS_PROFILE"]);

    let keys = AwsKeys {
        access_key_id: "AKIACONNNOTREAL".into(),
        secret_access_key: "conn-secret-not-real".into(),
        session_token: Some("conn-token-not-real".into()),
    };
    let env: BTreeMap<String, String> = child_env(&keys_conn(), Some(&keys), cache, parent_env())
        .into_iter()
        .collect();
    assert_eq!(
        env.get("AWS_ACCESS_KEY_ID").map(String::as_str),
        Some("AKIACONNNOTREAL")
    );
    assert_eq!(
        env.get("AWS_SECRET_ACCESS_KEY").map(String::as_str),
        Some("conn-secret-not-real")
    );
    assert_eq!(
        env.get("AWS_SESSION_TOKEN").map(String::as_str),
        Some("conn-token-not-real")
    );
    let aws: Vec<_> = env.keys().filter(|k| k.starts_with("AWS_")).collect();
    assert_eq!(
        aws,
        [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN"
        ]
    );
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

    let dir = wd.dir.clone();
    let writer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(60)).await;
        append(
            &dir.join("apply.log"),
            &(3..5).map(json_line).collect::<Vec<_>>(),
        );
        tokio::time::sleep(Duration::from_millis(60)).await;
        std::fs::write(dir.join("apply.exit"), "0").unwrap();
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

#[test]
fn no_run_is_in_progress_without_a_live_pid() {
    let tmp = tempfile::tempdir().unwrap();
    let wd = Workdir {
        dir: tmp.path().to_path_buf(),
    };
    assert!(!is_running(&wd));
    std::fs::write(wd.dir.join("apply.pid"), std::process::id().to_string()).unwrap();
    assert!(is_running(&wd), "this test process is alive");
    std::fs::write(wd.dir.join("apply.exit"), "0").unwrap();
    assert!(!is_running(&wd), "an exit code means the run is over");
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
    let cache = root.path().join("plugin-cache");
    std::fs::create_dir_all(&cache).unwrap();
    let env = child_env(&profile_conn("none"), None, &cache, std::env::vars());
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
