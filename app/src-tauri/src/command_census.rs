//! The command census shared by `build.rs` and the `cargo test` suite.
//!
//! `build.rs` pulls this file in with `#[path = "src/command_census.rs"]` and `lib.rs` with
//! `#[cfg(test)] mod command_census;`, so the parser that decides what the Tauri `AppManifest`
//! declares is the parser the tests exercise, and the rules that decide whether the build
//! passes have unit tests. Nothing here may reference the crate: only `std` and `serde_json`
//! (a dependency of both the crate and the build script).
//!
//! Spec: `docs/superpowers/specs/2026-09-22-app-manifest-lockdown-design.md` §3.2.

use std::collections::{BTreeMap, BTreeSet};

/// The command names inside `generate_handler![ … ])` in `lib.rs`, in registration order,
/// duplicates kept (the caller decides whether that is an error). `None` if the block is
/// missing or unterminated.
///
/// Comma-split, not line-split: `// Docker` style comments are stripped from every line first
/// (a whole-line comment strips to nothing; a trailing one leaves the code before it), and the
/// *cleaned* text is then split on `,` so each grant is its own item regardless of how many
/// share a line. A line-split version of this parser shipped first and used
/// `rsplit("::").next()` once *per line*: two commands on one line (`a::x, b::y,`) collapsed to
/// a single item, silently dropping `a::x` — a denied command at runtime with nothing flagging
/// it. Comma-splitting fixes that because it no longer assumes one item per line.
pub fn registered_commands(lib_rs: &str) -> Option<Vec<String>> {
    let (_, rest) = lib_rs.split_once("generate_handler![")?;
    let (inside, _) = rest.split_once("])")?;
    let cleaned: String = inside
        .lines()
        // Strip a trailing `//` comment (and a whole-line one, which strips to "").
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    Some(
        cleaned
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter_map(|s| {
                // `a::b::name` → `name`; a bare `name` (no `::`) is its own last segment.
                s.rsplit("::").next().map(|n| n.trim().to_string())
            })
            .filter(|n| !n.is_empty())
            .collect(),
    )
}

/// `viewer_read_file` → `allow-viewer-read-file`. tauri-utils 2.9.0 (`acl/build.rs:290`)
/// replaces only `_`; permission identifiers may not contain `_`, but the command name inside
/// the generated permission stays snake_case.
pub fn allow_permission(command: &str) -> String {
    format!("allow-{}", command.replace('_', "-"))
}

/// The `windows` list of the one capability file that may grant `command`. A command that
/// must be callable from both windows is a design change: make it here, visibly, rather than
/// by widening a capability file.
pub fn expected_windows(command: &str) -> &'static [&'static str] {
    if command.starts_with("viewer_") {
        &["file-viewer-*"]
    } else {
        &["main"]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityFile {
    pub name: String,
    pub windows: Vec<String>,
    pub bare: Vec<String>,
}

/// One `capabilities/*.json`, reduced to what the census checks. Plugin and core grants
/// (anything with a `:`) are not this module's business; the exact-set tests in `lib.rs` and
/// `file_viewer/mod.rs` pin those.
pub fn capability_file(name: &str, json: &str) -> Result<CapabilityFile, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("{name}: not valid JSON: {e}"))?;
    // `webviews` would extend the grants to webviews by label (the browser-view pop-out is
    // meant to be in no capability), and `remote` would extend them to a remote origin. The
    // census reasons about `windows` only, so either key is refused rather than half-checked.
    for key in ["webviews", "remote"] {
        if value.get(key).is_some() {
            return Err(format!(
                "{name}: `{key}` is not allowed; capabilities here are scoped by `windows` only"
            ));
        }
    }
    let windows = value["windows"]
        .as_array()
        .ok_or_else(|| format!("{name}: `windows` must be an array"))?
        .iter()
        .map(|w| {
            w.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{name}: `windows` entries must be strings"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut bare = Vec::new();
    for grant in value["permissions"]
        .as_array()
        .ok_or_else(|| format!("{name}: `permissions` must be an array"))?
    {
        let id = match grant {
            serde_json::Value::String(s) => s.as_str(),
            serde_json::Value::Object(o) => o
                .get("identifier")
                .and_then(|i| i.as_str())
                .ok_or_else(|| format!("{name}: a scoped grant needs a string `identifier`"))?,
            _ => return Err(format!("{name}: a grant is a string or an object")),
        };
        if !id.contains(':') {
            bare.push(id.to_string());
        }
    }
    Ok(CapabilityFile { name: name.to_string(), windows, bare })
}

/// Why an entry directly under `capabilities/` cannot be a capability the census reads, or
/// `None` if it is one (a top-level `*.json` file). tauri-build loads `capabilities/**/*` with
/// the extensions `json`, `toml` and (with a feature) `json5`, subdirectories included; the
/// census reads only top-level JSON, so anything else tauri might load is refused rather than
/// left for tauri to grant from unchecked. OS and editor junk, which tauri never loads, is the
/// caller's to skip first (see [`is_os_junk`]).
pub fn stray_capability_entry(name: &str, is_file: bool) -> Option<String> {
    if !is_file {
        return Some(format!(
            "capabilities/{name} is not a regular file; tauri loads capabilities from \
             subdirectories too, so every capability must be a top-level capabilities/*.json"
        ));
    }
    if name.ends_with(".json") {
        return None;
    }
    Some(format!(
        "capabilities/{name} is not a .json file; tauri may load it (it reads .toml and .json5 \
         too) but the census cannot check it, so every capability must be a top-level \
         capabilities/*.json"
    ))
}

/// Files the OS or an editor drops next to real ones (`.DS_Store`, `Thumbs.db`, `desktop.ini`,
/// Vim swap files, `name~` backups). tauri-build loads only `json`/`toml`/`json5` from
/// `capabilities/` and `permissions/`, so a junk name with one of those extensions (an Emacs
/// `.#default.json` lock, a macOS `._default.json`) is *not* junk: tauri would try to load it,
/// and the caller must refuse it.
pub fn is_os_junk(name: &str) -> bool {
    let loadable = [".json", ".json5", ".toml"].iter().any(|e| name.ends_with(e));
    !loadable
        && (matches!(name, ".DS_Store" | "Thumbs.db" | "desktop.ini")
            || name.ends_with(".swp")
            || name.ends_with(".swo")
            || name.ends_with('~'))
}

/// Which files next to `Cargo.toml` tauri reads as its config: `tauri.conf.json[5]`,
/// `Tauri.toml` and the per-platform `tauri.<platform>.conf.json[5]` / `Tauri.<platform>.toml`
/// (tauri-utils `config/parse.rs`). `Some(true)` = JSON the census can read, `Some(false)` = a
/// format it cannot (JSON5/TOML), `None` = not a tauri config file.
pub fn tauri_config_file(name: &str) -> Option<bool> {
    if name.starts_with("tauri.") && name.ends_with(".conf.json") {
        Some(true)
    } else if (name.starts_with("tauri.") && name.ends_with(".conf.json5"))
        || (name.starts_with("Tauri.") && name.ends_with(".toml"))
    {
        Some(false)
    } else {
        None
    }
}

/// A problem with a tauri config (a `tauri*.conf.json` file, or the `TAURI_CONFIG` JSON that
/// tauri-build merges over it), or `None`. `app.security.capabilities` is refused whenever it
/// is non-empty: an inline object is a capability the census never sees, and a list of
/// identifiers switches every *other* capability file off, which the census also assumes is
/// not happening.
pub fn tauri_config_problem(name: &str, json: &str) -> Option<String> {
    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => return Some(format!("{name}: not valid JSON: {e}")),
    };
    match value.pointer("/app/security/capabilities") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Array(a)) if a.is_empty() => None,
        Some(_) => Some(format!(
            "{name}: app.security.capabilities is not allowed; every capability lives in a \
             top-level capabilities/*.json file, where the census checks it"
        )),
    }
}

/// Everything that must hold between the handler list and the capability files. Returns every
/// violation rather than the first, so a batch of forgotten grants is one build failure; an
/// empty vector is a pass.
pub fn check(commands: &[String], files: &[CapabilityFile]) -> Vec<String> {
    let mut problems = Vec::new();
    if commands.is_empty() {
        problems.push(
            "no commands were parsed out of generate_handler! — an empty AppManifest would \
             silently leave every app command ungated"
                .to_string(),
        );
        return problems;
    }

    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for c in commands {
        if !c.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
            problems.push(format!("{c:?} is not a command name ([a-z0-9_]+)"));
        }
        if !seen.insert(c.as_str()) {
            problems.push(format!("{c} is registered more than once"));
        }
    }

    let known: BTreeMap<String, &str> =
        seen.iter().map(|c| (allow_permission(c), *c)).collect();
    for f in files {
        let windows: Vec<&str> = f.windows.iter().map(String::as_str).collect();
        for id in &f.bare {
            match known.get(id) {
                Some(command) => {
                    let want = expected_windows(command);
                    if windows.as_slice() != want {
                        problems.push(format!(
                            "{}: {id} must be granted in the capability file whose windows are \
                             {want:?}, not {windows:?}",
                            f.name
                        ));
                    }
                }
                None if id.starts_with("deny-") => problems.push(format!(
                    "{}: {id}: deny-* is global in tauri 2.11 — it would deny the command for \
                     every window, not just this one; use allow-lists only",
                    f.name
                )),
                None if id.starts_with("allow-") => problems.push(format!(
                    "{}: {id} names no registered command (the identifier is allow-<command> \
                     with every `_` replaced by `-`)",
                    f.name
                )),
                None => problems.push(format!(
                    "{}: {id}: only allow-<command> app grants are permitted as bare identifiers",
                    f.name
                )),
            }
        }
    }

    for c in &seen {
        let id = allow_permission(c);
        let holders: Vec<&str> = files
            .iter()
            .filter(|f| f.bare.iter().any(|b| b == &id))
            .map(|f| f.name.as_str())
            .collect();
        match holders.len() {
            0 => problems.push(format!(
                "{c} is registered but no capability file grants {id}; add it to the file \
                 whose windows are {:?}",
                expected_windows(c)
            )),
            1 => {}
            _ => problems.push(format!(
                "{id} is granted in more than one capability file: {holders:?}"
            )),
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmds(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn file(name: &str, windows: &[&str], bare: &[&str]) -> CapabilityFile {
        CapabilityFile {
            name: name.to_string(),
            windows: windows.iter().map(|w| w.to_string()).collect(),
            bare: bare.iter().map(|b| b.to_string()).collect(),
        }
    }

    /// The two files as they must look after the lockdown, for a three-command app.
    fn good_files() -> Vec<CapabilityFile> {
        vec![
            file("default.json", &["main"], &["allow-check-docker", "allow-open-file-viewer"]),
            file("file-viewer.json", &["file-viewer-*"], &["allow-viewer-read-file"]),
        ]
    }

    const THREE: &[&str] = &["check_docker", "open_file_viewer", "viewer_read_file"];

    #[test]
    fn the_parser_reads_the_handler_list_in_order_and_ignores_comments() {
        let lib_rs = r#"
            .invoke_handler(tauri::generate_handler![
                // Docker
                commands::docker_commands::check_docker,
                commands::docker_commands::build_image, // trailing comment is not a command
                url_open::open_url_external,

                // Viewer
                commands::file_viewer_commands::viewer_read_file
            ])
            .run(tauri::generate_context!())
        "#;
        assert_eq!(
            registered_commands(lib_rs).unwrap(),
            cmds(&["check_docker", "build_image", "open_url_external", "viewer_read_file"])
        );
    }

    #[test]
    fn the_parser_keeps_duplicates_so_the_caller_can_report_them() {
        let lib_rs = "generate_handler![\n a::x,\n b::x,\n])";
        assert_eq!(registered_commands(lib_rs).unwrap(), cmds(&["x", "x"]));
    }

    #[test]
    fn the_parser_returns_none_without_a_handler_block() {
        assert_eq!(registered_commands("fn main() {}"), None);
        assert_eq!(registered_commands("generate_handler![ a::b, "), None, "unterminated");
    }

    /// The bug this regression-tests: a line-split parser applies `rsplit("::").next()` once
    /// per *line*, so two commands sharing a line collapse into one item and the first is
    /// silently dropped. Comma-splitting must keep both regardless of layout.
    #[test]
    fn two_commands_on_one_line_are_both_kept() {
        let lib_rs = "generate_handler![\n a::x, b::y,\n])";
        assert_eq!(registered_commands(lib_rs).unwrap(), cmds(&["x", "y"]));
    }

    /// Mirrors the real `lib.rs` handler list's shape: `// Section` comments between groups,
    /// and command paths one (`open_url_external`), two (`url_open::open_url_external`) and
    /// three (`commands::docker_commands::check_docker`) segments deep, all ending in a comma
    /// except the last entry before `])`.
    #[test]
    fn a_fixture_shaped_like_the_real_handler_list_parses_every_command() {
        let lib_rs = r#"
            .invoke_handler(tauri::generate_handler![
                // Docker
                commands::docker_commands::check_docker,
                commands::docker_commands::build_image,
                // Opening a link in the host browser
                url_open::open_url_external,
                // Bare, module-less command
                open_help,
                // Terminal file viewer
                commands::file_viewer_commands::viewer_read_file
            ])
            .run(tauri::generate_context!())
        "#;
        assert_eq!(
            registered_commands(lib_rs).unwrap(),
            cmds(&[
                "check_docker",
                "build_image",
                "open_url_external",
                "open_help",
                "viewer_read_file",
            ])
        );
    }

    #[test]
    fn permission_identifiers_replace_only_underscores() {
        assert_eq!(allow_permission("check_docker"), "allow-check-docker");
        assert_eq!(allow_permission("viewer_read_file"), "allow-viewer-read-file");
        assert_eq!(allow_permission("aws_sso_refresh"), "allow-aws-sso-refresh");
    }

    #[test]
    fn viewer_commands_belong_to_the_viewer_windows_and_nothing_else_does() {
        assert_eq!(expected_windows("viewer_read_file"), ["file-viewer-*"]);
        assert_eq!(expected_windows("open_file_viewer"), ["main"]);
        assert_eq!(expected_windows("check_docker"), ["main"]);
    }

    #[test]
    fn a_capability_file_yields_its_windows_and_bare_grants_only() {
        let json = r#"{
            "identifier": "default",
            "description": "x",
            "windows": ["main"],
            "permissions": [
                "core:event:allow-listen",
                { "identifier": "fs:allow-read", "allow": [{ "path": "$APPDATA/*" }] },
                "allow-check-docker",
                { "identifier": "allow-list-projects" }
            ]
        }"#;
        let parsed = capability_file("default.json", json).unwrap();
        assert_eq!(parsed.name, "default.json");
        assert_eq!(parsed.windows, vec!["main"]);
        assert_eq!(parsed.bare, vec!["allow-check-docker", "allow-list-projects"]);
    }

    #[test]
    fn a_capability_file_without_windows_or_permissions_is_an_error() {
        assert!(capability_file("x.json", r#"{"permissions": []}"#).unwrap_err().contains("windows"));
        assert!(capability_file("x.json", r#"{"windows": ["main"]}"#).unwrap_err().contains("permissions"));
        assert!(capability_file("x.json", "not json").unwrap_err().contains("x.json"));
    }

    #[test]
    fn webviews_and_remote_keys_are_refused() {
        let with = |extra: &str| {
            format!(r#"{{"windows": ["main"], {extra}, "permissions": ["allow-check-docker"]}}"#)
        };
        let err = capability_file("d.json", &with(r#""webviews": ["browser-view-*"]"#)).unwrap_err();
        assert!(err.contains("d.json") && err.contains("`webviews`"), "{err}");
        let err = capability_file("d.json", &with(r#""remote": {"urls": ["https://*"]}"#)).unwrap_err();
        assert!(err.contains("`remote`"), "{err}");
        // Present-but-empty is still refused: the key itself is the widening surface.
        assert!(capability_file("d.json", &with(r#""webviews": []"#)).is_err());
    }

    #[test]
    fn only_top_level_json_files_are_capabilities() {
        assert_eq!(stray_capability_entry("default.json", true), None);
        for name in ["extra.toml", "extra.json5", "notes.txt", ".DS_Store"] {
            let err = stray_capability_entry(name, true).expect(name);
            assert!(err.contains(name) && err.contains("not a .json file"), "{err}");
        }
        let err = stray_capability_entry("sub", false).unwrap();
        assert!(err.contains("capabilities/sub") && err.contains("not a regular file"), "{err}");
        // A directory named like a capability is still a directory.
        assert!(stray_capability_entry("x.json", false).is_some());
    }

    #[test]
    fn os_junk_is_recognised_but_never_something_tauri_would_load() {
        for junk in [".DS_Store", "Thumbs.db", "desktop.ini", ".default.json.swp", ".x.swo", "default.json~"] {
            assert!(is_os_junk(junk), "{junk}");
        }
        for real in ["default.json", "x.toml", "x.json5", ".#default.json", "._default.json", "notes.txt", "extra"] {
            assert!(!is_os_junk(real), "{real}");
        }
    }

    #[test]
    fn tauri_config_files_are_found_by_name_and_format() {
        assert_eq!(tauri_config_file("tauri.conf.json"), Some(true));
        assert_eq!(tauri_config_file("tauri.linux.conf.json"), Some(true));
        assert_eq!(tauri_config_file("tauri.conf.json5"), Some(false));
        assert_eq!(tauri_config_file("tauri.windows.conf.json5"), Some(false));
        assert_eq!(tauri_config_file("Tauri.toml"), Some(false));
        assert_eq!(tauri_config_file("Tauri.macos.toml"), Some(false));
        assert_eq!(tauri_config_file("Cargo.toml"), None);
        assert_eq!(tauri_config_file("build.rs"), None);
    }

    #[test]
    fn inline_capabilities_in_the_tauri_config_are_refused() {
        let ok = r#"{"app": {"security": {"csp": "default-src 'self'"}}}"#;
        assert_eq!(tauri_config_problem("tauri.conf.json", ok), None);
        assert_eq!(tauri_config_problem("t", r#"{"app": {"security": {"capabilities": []}}}"#), None);
        assert_eq!(tauri_config_problem("t", r#"{"build": {"beforeBuildCommand": ""}}"#), None);
        let inline = r#"{"app": {"security": {"capabilities": [
            {"identifier": "x", "windows": ["file-viewer-*"], "permissions": ["allow-read-container-file"]}
        ]}}}"#;
        let err = tauri_config_problem("tauri.conf.json", inline).unwrap();
        assert!(err.contains("tauri.conf.json") && err.contains("app.security.capabilities"), "{err}");
        let by_name = r#"{"app": {"security": {"capabilities": ["default"]}}}"#;
        assert!(tauri_config_problem("TAURI_CONFIG", by_name).unwrap().contains("TAURI_CONFIG"));
        assert!(tauri_config_problem("t", "{").unwrap().contains("not valid JSON"));
    }

    #[test]
    fn a_correct_census_has_no_problems() {
        assert_eq!(check(&cmds(THREE), &good_files()), Vec::<String>::new());
    }

    #[test]
    fn an_empty_command_list_is_refused_because_it_would_disable_the_acl() {
        let problems = check(&[], &good_files());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("no commands"), "{problems:?}");
    }

    #[test]
    fn a_command_without_a_grant_is_named_together_with_the_file_it_belongs_in() {
        let files = vec![
            file("default.json", &["main"], &["allow-check-docker"]),
            file("file-viewer.json", &["file-viewer-*"], &["allow-viewer-read-file"]),
        ];
        let problems = check(&cmds(THREE), &files);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("open_file_viewer"));
        assert!(problems[0].contains("allow-open-file-viewer"));
        assert!(problems[0].contains("[\"main\"]"));
    }

    #[test]
    fn a_grant_in_two_files_is_reported_once_naming_both() {
        let files = vec![
            file("default.json", &["main"], &["allow-check-docker", "allow-open-file-viewer"]),
            file("extra.json", &["main"], &["allow-check-docker"]),
            file("file-viewer.json", &["file-viewer-*"], &["allow-viewer-read-file"]),
        ];
        let problems = check(&cmds(THREE), &files);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("allow-check-docker"));
        assert!(problems[0].contains("default.json") && problems[0].contains("extra.json"));
    }

    #[test]
    fn a_grant_that_names_no_command_is_a_typo() {
        let mut files = good_files();
        files[0].bare.push("allow-check-dokcer".to_string());
        let problems = check(&cmds(THREE), &files);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("default.json: allow-check-dokcer"));
        assert!(problems[0].contains("no registered command"));
    }

    #[test]
    fn deny_grants_are_refused_with_the_reason() {
        let mut files = good_files();
        files[1].bare.push("deny-check-docker".to_string());
        let problems = check(&cmds(THREE), &files);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("file-viewer.json: deny-check-docker"));
        assert!(problems[0].contains("global"));
    }

    #[test]
    fn other_bare_identifiers_are_refused() {
        let mut files = good_files();
        files[0].bare.push("default".to_string());
        let problems = check(&cmds(THREE), &files);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("default.json: default"));
    }

    #[test]
    fn a_grant_in_the_wrong_file_is_refused_even_though_it_is_granted_exactly_once() {
        let files = vec![
            file("default.json", &["main"], &["allow-check-docker", "allow-open-file-viewer", "allow-viewer-read-file"]),
            file("file-viewer.json", &["file-viewer-*"], &[]),
        ];
        let problems = check(&cmds(THREE), &files);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("allow-viewer-read-file"));
        assert!(problems[0].contains("[\"file-viewer-*\"]"));
    }

    #[test]
    fn a_widened_windows_list_is_the_wrong_file_too() {
        let files = vec![
            file("default.json", &["main", "file-viewer-*"], &["allow-check-docker", "allow-open-file-viewer"]),
            file("file-viewer.json", &["file-viewer-*"], &["allow-viewer-read-file"]),
        ];
        let problems = check(&cmds(THREE), &files);
        assert_eq!(problems.len(), 2, "{problems:?}");
    }

    #[test]
    fn bad_names_and_duplicate_registrations_are_refused() {
        let commands = cmds(&["check_docker", "Check-Docker", "check_docker", "open_file_viewer", "viewer_read_file"]);
        let problems = check(&commands, &good_files());
        assert!(problems.iter().any(|p| p.contains("\"Check-Docker\"") && p.contains("[a-z0-9_]+")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("check_docker is registered more than once")), "{problems:?}");
    }

    #[test]
    fn every_problem_is_reported_in_one_pass() {
        let files = vec![
            file("default.json", &["main"], &["allow-check-docker", "allow-nope", "deny-check-docker"]),
            file("file-viewer.json", &["file-viewer-*"], &[]),
        ];
        let problems = check(&cmds(THREE), &files);
        // typo, deny, open_file_viewer missing, viewer_read_file missing
        assert_eq!(problems.len(), 4, "{problems:?}");
    }
}
