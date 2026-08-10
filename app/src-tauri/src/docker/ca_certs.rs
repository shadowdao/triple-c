//! Corporate CA certificate injection.
//!
//! Users behind a TLS-terminating corporate proxy need their organisation's
//! root CA inside every container, or **every** HTTPS call fails — npm, pip,
//! git, curl, the Playwright browser, and Claude Code's own API calls.
//!
//! The mechanism follows the SSH/AWS host-mount pattern in [`super::container`]:
//! a host path is bind-mounted **read-only** into the container and
//! `entrypoint.sh` applies it on every start. That is what makes it durable
//! across container recreation, base-image migration and Reset — a certificate
//! installed by hand inside a running container is lost the first time any of
//! those happen.
//!
//! ## Two things that are easy to get wrong
//!
//! 1. **`update-ca-certificates` only reads `*.crt`.** It globs
//!    `/usr/local/share/ca-certificates/*.crt` case-sensitively, so a `.pem`
//!    (the far more common export format) that is merely *copied* in is
//!    silently ignored — no warning, no error, just a container that still
//!    cannot speak HTTPS. Certificates must be **renamed**, which is what
//!    [`container_cert_name`] does.
//!
//! 2. **The system trust store is not enough.** Only curl/git/apt read it.
//!    Node — and therefore Claude Code itself — needs `NODE_EXTRA_CA_CERTS`,
//!    Python/requests need `REQUESTS_CA_BUNDLE`/`SSL_CERT_FILE`, and
//!    Chrome/Chromium read neither: they have their own NSS database at
//!    `~/.pki/nssdb`, seeded by `certutil` in the entrypoint.
//!
//! ## Why the env vars are set from Rust and not exported by the entrypoint
//!
//! An `export` in `entrypoint.sh` reaches only the entrypoint's own children.
//! Every terminal session is a separate `docker exec`, which inherits the
//! *container's* configured env and sees nothing the entrypoint exported —
//! the same lesson that forced `$BROWSER` to become an image-level `ENV` for
//! the URL relay shim. Since the bundle path written by
//! `update-ca-certificates` is deterministic ([`CA_BUNDLE_PATH`]), Rust can set
//! all three vars at container creation, where `docker exec` will see them.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Where the host's CA material is bind-mounted, read-only. Mirrors
/// `/tmp/.host-ssh` and `/tmp/.host-aws`.
///
/// A *directory* on the host is mounted here as-is. A single *file* is mounted
/// at `<CA_MOUNT_DIR>/<normalised name>` — Docker creates the parent — so the
/// entrypoint only ever has to deal with a directory, and the certificate keeps
/// a recognisable name instead of becoming the literal path `.host-ca`.
pub const CA_MOUNT_DIR: &str = "/tmp/.host-ca";

/// The concatenated PEM bundle `update-ca-certificates` writes on
/// Debian/Ubuntu. Deterministic, which is what lets the env vars below be set
/// at container-creation time, before the entrypoint has run.
pub const CA_BUNDLE_PATH: &str = "/etc/ssl/certs/ca-certificates.crt";

/// Consulted by Node — and therefore by Claude Code itself, which is the whole
/// reason this feature exists.
pub const NODE_EXTRA_CA_CERTS: &str = "NODE_EXTRA_CA_CERTS";
/// Consulted by `requests` (and so by pip's vendored copy).
pub const REQUESTS_CA_BUNDLE: &str = "REQUESTS_CA_BUNDLE";
/// Consulted by OpenSSL, and so by Python's `ssl` module.
pub const SSL_CERT_FILE: &str = "SSL_CERT_FILE";

/// Every env var this module owns, in a fixed order.
///
/// Also the list that must be *cleared* when no CA is configured: `docker
/// commit` bakes a container's env into the project's snapshot image, and
/// create-time env replaces image `ENV` per key — so without an explicit empty
/// value, removing the setting would leave the vars live in every future
/// container. Empty is safe for all three (verified on Ubuntu 24.04: curl,
/// `openssl s_client` and Python's `ssl` all behave exactly as they do with the
/// variable unset).
pub const CA_ENV_KEYS: &[&str] = &[NODE_EXTRA_CA_CERTS, REQUESTS_CA_BUNDLE, SSL_CERT_FILE];

/// Extensions treated as certificates when the configured path is a directory.
/// Matched case-insensitively. DER is deliberately absent — the system store
/// and every consumer here want PEM.
const CERT_EXTENSIONS: &[&str] = &["crt", "pem", "cer", "cert", "ca-bundle"];

/// A configured CA path that has been checked and resolved into everything the
/// container creation path needs.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCa {
    /// The host path, as configured.
    pub host_path: String,
    /// Whether the host path is a directory (as opposed to a single file).
    pub is_dir: bool,
    /// The bind-mount target inside the container.
    pub mount_target: String,
    /// The certificate files found, sorted.
    pub cert_files: Vec<PathBuf>,
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// The file name a certificate is installed as under
/// `/usr/local/share/ca-certificates/`.
///
/// `update-ca-certificates` globs `*.crt` **case-sensitively**, so `.pem`,
/// `.cer`, `.CRT` and extension-less files all have to end up as a lowercase
/// `.crt` or they are ignored without a word. Characters outside
/// `[A-Za-z0-9._-]` are replaced so that whitespace cannot break the shell
/// loops that walk the store, and leading dots are stripped so a hidden file
/// does not stay hidden.
///
/// `entrypoint.sh` reimplements exactly this in a few lines of shell (it has to
/// rename the files inside the container); the two must agree, which is what
/// the unit tests below pin down.
pub fn container_cert_name(file_name: &str) -> String {
    let sanitized: String = file_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let sanitized = sanitized.trim_start_matches('.');
    // Strip one trailing extension, whatever it is, then force `.crt`. A name
    // with no dot keeps its whole self as the stem.
    let stem = match sanitized.rfind('.') {
        Some(i) => &sanitized[..i],
        None => sanitized,
    };
    let stem = if stem.is_empty() { "corporate-ca" } else { stem };
    format!("{}.crt", stem)
}

/// Whether a directory entry looks like a certificate worth installing.
fn is_cert_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    CERT_EXTENSIONS.contains(&ext.as_str())
}

/// The certificate files a configured path contributes.
///
/// A file is taken at face value — the user pointed at it explicitly, so its
/// extension is not second-guessed. A directory is scanned one level deep
/// (matching the entrypoint's `find -maxdepth 1`) and filtered by extension,
/// so an `openssl.cnf` or a README sitting next to the certs is skipped.
/// The result is sorted, so the fingerprint is stable across filesystem
/// enumeration order.
pub fn collect_cert_files(path: &Path) -> Vec<PathBuf> {
    if path.is_file() {
        return vec![path.to_path_buf()];
    }
    if !path.is_dir() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_cert_file(p))
        .collect();
    files.sort();
    files
}

/// Resolve the configured CA path, or explain why it cannot be used.
///
/// `Ok(None)` means "no CA configured", which is the overwhelmingly common
/// case and must stay free. An `Err` aborts the container start: behind a
/// TLS-intercepting proxy a container without the CA is broken in a dozen
/// confusing ways, so naming the bad path once is far kinder than letting npm,
/// pip and Claude Code each fail their own way.
pub fn resolve(path: Option<&str>) -> Result<Option<ResolvedCa>, String> {
    let Some(raw) = path.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let root = Path::new(raw);
    if !root.exists() {
        return Err(format!(
            "Corporate CA certificate path '{}' does not exist. Update it in \
             Settings → Certificates, or clear this project's override in \
             Project Home → Config → Access.",
            raw
        ));
    }

    let is_dir = root.is_dir();
    if !is_dir && !root.is_file() {
        return Err(format!(
            "Corporate CA certificate path '{}' is neither a file nor a directory.",
            raw
        ));
    }

    let cert_files = collect_cert_files(root);
    if cert_files.is_empty() {
        return Err(format!(
            "Corporate CA certificate directory '{}' contains no certificate files \
             (looked for {} one level deep).",
            raw,
            CERT_EXTENSIONS
                .iter()
                .map(|e| format!(".{}", e))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let mount_target = if is_dir {
        CA_MOUNT_DIR.to_string()
    } else {
        let name = root
            .file_name()
            .map(|n| container_cert_name(&n.to_string_lossy()))
            .unwrap_or_else(|| "corporate-ca.crt".to_string());
        format!("{}/{}", CA_MOUNT_DIR, name)
    };

    Ok(Some(ResolvedCa {
        host_path: raw.to_string(),
        is_dir,
        mount_target,
        cert_files,
    }))
}

/// Fingerprint of the CA configuration, for the `triple-c.ca-fingerprint`
/// label.
///
/// `container_needs_recreation` is label-based and never diffs env or mounts,
/// so without this, changing the CA path would silently do nothing until some
/// unrelated setting forced a rebuild.
///
/// It covers **both** the resolved path *and the bytes of every certificate*,
/// because replacing a rotated CA at the same path is at least as common as
/// moving it — and the container's copy is made once, at start, so nothing else
/// would notice.
///
/// Never returns an error: a path that has gone missing hashes differently from
/// one that is present, which is exactly the "something changed, recreate"
/// signal wanted here. Reporting the problem is [`resolve`]'s job.
pub fn compute_ca_fingerprint(path: Option<&str>) -> String {
    let Some(raw) = path.map(str::trim).filter(|s| !s.is_empty()) else {
        return String::new();
    };
    let mut parts: Vec<String> = vec![raw.to_string()];
    let root = Path::new(raw);
    if !root.exists() {
        parts.push("<missing>".to_string());
    } else {
        for file in collect_cert_files(root) {
            let name = file
                .file_name()
                .map(|n| container_cert_name(&n.to_string_lossy()))
                .unwrap_or_default();
            let digest = match std::fs::read(&file) {
                Ok(bytes) => {
                    let mut hasher = Sha256::new();
                    hasher.update(&bytes);
                    format!("{:x}", hasher.finalize())
                }
                Err(_) => "<unreadable>".to_string(),
            };
            parts.push(format!("{}:{}", name, digest));
        }
    }
    sha256_hex(&parts.join("|"))
}

/// The env vars to set on the container.
///
/// Always returns all of [`CA_ENV_KEYS`]: pointing at the bundle when a CA is
/// configured, empty when it is not. The empty case is not cosmetic — see the
/// note on [`CA_ENV_KEYS`].
pub fn ca_env_vars(resolved: Option<&ResolvedCa>) -> Vec<(&'static str, String)> {
    let value = if resolved.is_some() { CA_BUNDLE_PATH } else { "" };
    CA_ENV_KEYS
        .iter()
        .map(|key| (*key, value.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A scratch directory that cleans itself up. `tempfile` is not a
    /// dependency of this crate and this is the only test that needs one.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "triple-c-ca-test-{}-{}-{:?}",
                tag,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        fn write(&self, name: &str, contents: &str) -> PathBuf {
            let p = self.0.join(name);
            fs::write(&p, contents).unwrap();
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    // ── container_cert_name ────────────────────────────────────────────────

    #[test]
    fn a_pem_is_renamed_to_crt_not_merely_copied() {
        // The whole point: update-ca-certificates globs *.crt and would
        // silently ignore corp-root.pem.
        assert_eq!(container_cert_name("corp-root.pem"), "corp-root.crt");
    }

    #[test]
    fn a_crt_keeps_its_name() {
        assert_eq!(container_cert_name("corp-root.crt"), "corp-root.crt");
    }

    #[test]
    fn other_certificate_extensions_are_renamed_too() {
        assert_eq!(container_cert_name("zscaler.cer"), "zscaler.crt");
        assert_eq!(container_cert_name("zscaler.cert"), "zscaler.crt");
        assert_eq!(container_cert_name("bundle.ca-bundle"), "bundle.crt");
    }

    #[test]
    fn an_uppercase_extension_is_lowercased() {
        // `find -name '*.crt'` is case-sensitive, so CA.CRT would be ignored.
        assert_eq!(container_cert_name("CA.CRT"), "CA.crt");
        assert_eq!(container_cert_name("CA.PEM"), "CA.crt");
    }

    #[test]
    fn a_name_without_an_extension_gains_one() {
        assert_eq!(container_cert_name("corporate-root"), "corporate-root.crt");
    }

    #[test]
    fn only_the_last_extension_is_replaced() {
        assert_eq!(container_cert_name("corp.root.ca.pem"), "corp.root.ca.crt");
    }

    #[test]
    fn unsafe_characters_are_replaced() {
        assert_eq!(
            container_cert_name("Corp Root CA (2026).pem"),
            "Corp_Root_CA__2026_.crt"
        );
        assert_eq!(container_cert_name("a/b.pem"), "a_b.crt");
    }

    #[test]
    fn leading_dots_are_stripped_so_the_file_is_not_hidden() {
        assert_eq!(container_cert_name(".hidden.pem"), "hidden.crt");
    }

    #[test]
    fn a_degenerate_name_still_produces_a_usable_file() {
        assert_eq!(container_cert_name(".pem"), "pem.crt");
        assert_eq!(container_cert_name(""), "corporate-ca.crt");
        assert_eq!(container_cert_name("..."), "corporate-ca.crt");
    }

    #[test]
    fn every_produced_name_ends_in_lowercase_crt() {
        for input in [
            "a.pem", "b.CRT", "c", ".d.pem", "", "e f.cer", "...", "ç.pem",
        ] {
            let out = container_cert_name(input);
            assert!(
                out.ends_with(".crt"),
                "{:?} produced {:?}, which update-ca-certificates would ignore",
                input,
                out
            );
            assert!(
                out.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'),
                "{:?} produced {:?}, which is not shell-safe",
                input,
                out
            );
        }
    }

    // ── fingerprint ────────────────────────────────────────────────────────

    #[test]
    fn no_configured_path_fingerprints_as_empty() {
        assert_eq!(compute_ca_fingerprint(None), "");
        assert_eq!(compute_ca_fingerprint(Some("")), "");
        assert_eq!(compute_ca_fingerprint(Some("   ")), "");
    }

    #[test]
    fn changing_the_path_changes_the_fingerprint() {
        let a = TempDir::new("path-a");
        let b = TempDir::new("path-b");
        // Identical *content* in both, so only the path differs.
        a.write("corp.pem", "CERT-BODY");
        b.write("corp.pem", "CERT-BODY");

        let fp_a = compute_ca_fingerprint(Some(a.path().to_str().unwrap()));
        let fp_b = compute_ca_fingerprint(Some(b.path().to_str().unwrap()));
        assert_ne!(fp_a, "");
        assert_ne!(
            fp_a, fp_b,
            "two different paths must not share a fingerprint"
        );
    }

    #[test]
    fn changing_the_certificate_content_at_the_same_path_changes_the_fingerprint() {
        // The case a path-only fingerprint would miss: the corporate CA is
        // rotated and the new one dropped in at exactly the same location.
        let dir = TempDir::new("rotate");
        dir.write("corp.pem", "OLD-CERT");
        let before = compute_ca_fingerprint(Some(dir.path().to_str().unwrap()));

        dir.write("corp.pem", "NEW-CERT");
        let after = compute_ca_fingerprint(Some(dir.path().to_str().unwrap()));

        assert_ne!(
            before, after,
            "replacing the certificate at the same path must force a recreation"
        );
    }

    #[test]
    fn adding_or_removing_a_certificate_changes_the_fingerprint() {
        let dir = TempDir::new("add");
        dir.write("one.pem", "A");
        let one = compute_ca_fingerprint(Some(dir.path().to_str().unwrap()));
        dir.write("two.pem", "B");
        let two = compute_ca_fingerprint(Some(dir.path().to_str().unwrap()));
        assert_ne!(one, two);
        fs::remove_file(dir.path().join("two.pem")).unwrap();
        assert_eq!(compute_ca_fingerprint(Some(dir.path().to_str().unwrap())), one);
    }

    #[test]
    fn an_unchanged_directory_fingerprints_identically() {
        let dir = TempDir::new("stable");
        dir.write("corp.pem", "SAME");
        let a = compute_ca_fingerprint(Some(dir.path().to_str().unwrap()));
        let b = compute_ca_fingerprint(Some(dir.path().to_str().unwrap()));
        assert_eq!(a, b, "the fingerprint must not churn on repeated reads");
    }

    #[test]
    fn a_missing_path_fingerprints_differently_from_a_present_one() {
        let dir = TempDir::new("missing");
        let present = compute_ca_fingerprint(Some(dir.path().to_str().unwrap()));
        let missing =
            compute_ca_fingerprint(Some(&format!("{}-gone", dir.path().to_str().unwrap())));
        assert_ne!(present, missing);
        assert_ne!(missing, "");
    }

    #[test]
    fn non_certificate_files_in_the_directory_are_ignored() {
        let dir = TempDir::new("noise");
        dir.write("corp.pem", "CERT");
        let before = compute_ca_fingerprint(Some(dir.path().to_str().unwrap()));
        dir.write("README.md", "hello");
        dir.write("openssl.cnf", "[req]");
        assert_eq!(
            compute_ca_fingerprint(Some(dir.path().to_str().unwrap())),
            before
        );
    }

    // ── resolve ────────────────────────────────────────────────────────────

    #[test]
    fn no_path_resolves_to_nothing() {
        assert_eq!(resolve(None).unwrap(), None);
        assert_eq!(resolve(Some("  ")).unwrap(), None);
    }

    #[test]
    fn a_missing_path_is_an_actionable_error() {
        let err = resolve(Some("/definitely/not/here/corp.pem")).unwrap_err();
        assert!(err.contains("/definitely/not/here/corp.pem"), "{}", err);
        assert!(err.contains("Settings"), "{}", err);
    }

    #[test]
    fn an_empty_directory_is_an_actionable_error() {
        let dir = TempDir::new("empty");
        let err = resolve(Some(dir.path().to_str().unwrap())).unwrap_err();
        assert!(err.contains("no certificate files"), "{}", err);
        assert!(err.contains(".pem"), "{}", err);
    }

    #[test]
    fn a_directory_mounts_at_the_shared_mount_point() {
        let dir = TempDir::new("dir");
        dir.write("corp.pem", "CERT");
        let resolved = resolve(Some(dir.path().to_str().unwrap())).unwrap().unwrap();
        assert!(resolved.is_dir);
        assert_eq!(resolved.mount_target, CA_MOUNT_DIR);
        assert_eq!(resolved.cert_files.len(), 1);
    }

    #[test]
    fn a_single_file_mounts_under_the_mount_point_with_a_crt_name() {
        // Mounting a file *at* /tmp/.host-ca would leave the entrypoint with no
        // name to work from, and would make the mount point a file rather than
        // the directory the entrypoint expects.
        let dir = TempDir::new("file");
        let file = dir.write("corp root.pem", "CERT");
        let resolved = resolve(Some(file.to_str().unwrap())).unwrap().unwrap();
        assert!(!resolved.is_dir);
        assert_eq!(
            resolved.mount_target,
            format!("{}/corp_root.crt", CA_MOUNT_DIR)
        );
    }

    #[test]
    fn a_file_is_accepted_whatever_its_extension() {
        // The user pointed at it explicitly; don't second-guess.
        let dir = TempDir::new("odd-ext");
        let file = dir.write("corp.txt", "CERT");
        let resolved = resolve(Some(file.to_str().unwrap())).unwrap().unwrap();
        assert_eq!(resolved.cert_files, vec![file]);
    }

    // ── env vars ───────────────────────────────────────────────────────────

    #[test]
    fn configured_ca_points_every_consumer_at_the_bundle() {
        let dir = TempDir::new("env");
        dir.write("corp.pem", "CERT");
        let resolved = resolve(Some(dir.path().to_str().unwrap())).unwrap();
        let vars = ca_env_vars(resolved.as_ref());
        assert_eq!(
            vars,
            vec![
                (NODE_EXTRA_CA_CERTS, CA_BUNDLE_PATH.to_string()),
                (REQUESTS_CA_BUNDLE, CA_BUNDLE_PATH.to_string()),
                (SSL_CERT_FILE, CA_BUNDLE_PATH.to_string()),
            ]
        );
    }

    #[test]
    fn no_ca_clears_every_var_rather_than_omitting_it() {
        // Omitting them would let a value baked into the project's snapshot
        // image survive the setting being turned off.
        let vars = ca_env_vars(None);
        assert_eq!(vars.len(), CA_ENV_KEYS.len());
        assert!(vars.iter().all(|(_, v)| v.is_empty()));
    }
}
