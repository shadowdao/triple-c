use tauri::{Emitter, State};

use crate::commands::aws_commands;
use crate::docker;
use crate::models::{container_config, AppSettings, Backend, BedrockAuthMethod, Project, ProjectPath, ProjectRemovalReport, ProjectResetOutcome, ProjectStatus};
use crate::storage::secure;
use crate::AppState;

pub(crate) fn emit_progress(app_handle: &tauri::AppHandle, project_id: &str, message: &str) {
    let _ = app_handle.emit(
        "container-progress",
        serde_json::json!({
            "project_id": project_id,
            "message": message,
        }),
    );
}

/// Every project secret, as the JSON pointer it arrives under and the keychain
/// key it is stored as.
///
/// The keychain keys are the ones already in users' keychains — changing one
/// orphans the secret rather than migrating it.
const PROJECT_SECRET_FIELDS: &[(&str, &str)] = &[
    ("/git_token", "git-token"),
    ("/bedrock_config/aws_access_key_id", "aws-access-key-id"),
    ("/bedrock_config/aws_secret_access_key", "aws-secret-access-key"),
    ("/bedrock_config/aws_session_token", "aws-session-token"),
    ("/bedrock_config/aws_bearer_token", "aws-bearer-token"),
    ("/openai_compatible_config/api_key", "openai-compatible-api-key"),
];

/// The keychain keys the caller sent an explicit `null` for.
///
/// **Absent and `null` are different things here, and treating them alike
/// destroys credentials.** Every secret field is `#[serde(skip_serializing)]`,
/// so the `Project` the frontend holds has no `git_token` key at all — a save
/// from the Workspace, Runtime or Model section spreads that object and sends
/// the field *absent*, while the editor that owns the field sends
/// `git_token: null` when the user blanks it (`AccessSection.tsx`:
/// `save({ git_token: gitToken || null })`). Serde maps both to `None`, which
/// is why this reads the payload rather than the deserialised struct: absent
/// means "not mine to touch", `null` means "the user emptied it".
fn explicitly_cleared_secrets(payload: &serde_json::Value) -> Vec<&'static str> {
    PROJECT_SECRET_FIELDS
        .iter()
        .filter(|(pointer, _)| matches!(payload.pointer(pointer), Some(serde_json::Value::Null)))
        .map(|(_, key)| *key)
        .collect()
}

/// Store one secret, clear it, or leave it alone — see
/// [`explicitly_cleared_secrets`] for which is which.
fn save_secret(
    project_id: &str,
    key_name: &str,
    value: Option<&str>,
    explicitly_cleared: &[&str],
) -> Result<(), String> {
    if value.is_none() && !explicitly_cleared.contains(&key_name) {
        return Ok(());
    }
    secure::store_or_clear_project_secret(project_id, key_name, value)
}

/// Extract secret fields from a project and store them in the OS keychain,
/// **deleting the ones the caller blanked**.
///
/// The `if let Some(v) = …` this replaces could only ever write: a blanked
/// token left the old value in the keychain, `load_secrets_for_project` read it
/// straight back onto the project, and the container went on getting the
/// credential the user had just revoked.
fn store_secrets_for_project(project: &Project, explicitly_cleared: &[&str]) -> Result<(), String> {
    save_secret(
        &project.id,
        "git-token",
        project.git_token.as_deref(),
        explicitly_cleared,
    )?;
    if let Some(ref bedrock) = project.bedrock_config {
        save_secret(
            &project.id,
            "aws-access-key-id",
            bedrock.aws_access_key_id.as_deref(),
            explicitly_cleared,
        )?;
        save_secret(
            &project.id,
            "aws-secret-access-key",
            bedrock.aws_secret_access_key.as_deref(),
            explicitly_cleared,
        )?;
        save_secret(
            &project.id,
            "aws-session-token",
            bedrock.aws_session_token.as_deref(),
            explicitly_cleared,
        )?;
        save_secret(
            &project.id,
            "aws-bearer-token",
            bedrock.aws_bearer_token.as_deref(),
            explicitly_cleared,
        )?;
    }
    if let Some(ref oai_config) = project.openai_compatible_config {
        save_secret(
            &project.id,
            "openai-compatible-api-key",
            oai_config.api_key.as_deref(),
            explicitly_cleared,
        )?;
    }
    Ok(())
}

/// Create the project's container, threading every global setting through.
///
/// Exists so that the two ordinary create paths below and base-image migration
/// cannot drift apart — a container created by a migration must be
/// indistinguishable from one created by a normal start, or the next
/// `container_needs_recreation` would immediately throw it away.
///
/// `create_image` is what to create *from* (the snapshot or the base);
/// `base_image_name` is the configured base, which `create_container` needs in
/// order to tell those two apart when it stamps the lineage labels.
pub(crate) async fn create_container_for_project(
    project: &Project,
    settings: &AppSettings,
    docker_socket: &str,
    aws_config_path: Option<&str>,
    create_image: &str,
    base_image_name: &str,
    extras: docker::CreateExtras<'_>,
) -> Result<String, String> {
    docker::create_container(
        project,
        docker_socket,
        create_image,
        base_image_name,
        extras,
        aws_config_path,
        &settings.global_aws,
        &settings.global_ollama,
        &settings.global_llamacpp,
        &settings.global_openai_compatible,
        settings.global_claude_instructions.as_deref(),
        &settings.global_custom_env_vars,
        settings.timezone.as_deref(),
        settings.global_claude_code_settings.as_ref(),
        settings.default_ssh_key_path.as_deref(),
        settings.ca_cert_path.as_deref(),
        settings.default_git_user_name.as_deref(),
        settings.default_git_user_email.as_deref(),
    )
    .await
}

/// Populate secret fields on a project struct from the OS keychain.
pub(crate) fn load_secrets_for_project(project: &mut Project) {
    project.git_token = secure::get_project_secret(&project.id, "git-token")
        .unwrap_or(None);
    if let Some(ref mut bedrock) = project.bedrock_config {
        bedrock.aws_access_key_id = secure::get_project_secret(&project.id, "aws-access-key-id")
            .unwrap_or(None);
        bedrock.aws_secret_access_key = secure::get_project_secret(&project.id, "aws-secret-access-key")
            .unwrap_or(None);
        bedrock.aws_session_token = secure::get_project_secret(&project.id, "aws-session-token")
            .unwrap_or(None);
        bedrock.aws_bearer_token = secure::get_project_secret(&project.id, "aws-bearer-token")
            .unwrap_or(None);
    }
    if let Some(ref mut oai_config) = project.openai_compatible_config {
        oai_config.api_key = secure::get_project_secret(&project.id, "openai-compatible-api-key")
            .unwrap_or(None);
    }
}

/// Validate the folder list a project is about to be stored with.
///
/// ## Why this is not cosmetic
///
/// `mount_name` is interpolated straight into a container mount target —
/// `docker::create_container` builds `/workspace/{mount_name}` — and the daemon
/// **normalises** what it is given. Confirmed against Engine 29.7 through the
/// same API bollard uses: a mount named `../tmp/claude-x` is created with a
/// destination of `/tmp/claude-x`, i.e. the host directory is mounted straight
/// on top of one of the paths the pre-commit scrub owns. The next recreate then
/// runs the scrub, as root, over the user's own project directory. That is the
/// C1 data-loss chain end to end, and
/// [`check_mount_name_stays_under_workspace`] is the half of it that stops the
/// path ever being spelled.
///
/// `host_path` is the other side of the same mount. `/` there bind-mounts the
/// entire host filesystem read-write into a container whose agent has
/// passwordless sudo. Anything short of a filesystem root is the user choosing
/// a folder — the Browse button and the free-text field lead to the same place
/// — so only the roots themselves are refused, and *root* is answered by
/// resolving the path rather than by reading it: `/..` is spelled like a folder
/// and is the root. See [`classify_mount_source`].
///
/// This is the **whole** rule set, and it belongs to `add_project`, where every
/// row is new by definition. `update_project` runs
/// [`validate_project_paths_update`] instead: see there for why a list that is
/// already in `projects.json` cannot be held to all of it.
fn validate_project_paths(paths: &[ProjectPath]) -> Result<(), String> {
    let mut seen_names = std::collections::HashSet::new();
    for p in paths {
        // The Config tab's "+ Add folder" inserts an empty row and saves the
        // whole list on the next blur, so a wholly blank entry is the UI's
        // placeholder rather than an attempt at anything. It is stored as it
        // always was; a half-filled one is refused.
        if p.host_path.is_empty() && p.mount_name.is_empty() {
            continue;
        }
        validate_one_path(p)?;
        if !seen_names.insert(p.mount_name.clone()) {
            return Err(format!("Duplicate mount name '{}'.", p.mount_name));
        }
    }
    Ok(())
}

/// Every rule that applies to a single folder row, duplicates aside.
fn validate_one_path(p: &ProjectPath) -> Result<(), String> {
    if p.mount_name.is_empty() {
        return Err("Mount name cannot be empty.".to_string());
    }
    if !p.mount_name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return Err(format!("Mount name '{}' contains invalid characters. Use alphanumeric, dash, underscore, or dot.", p.mount_name));
    }
    check_mount_name_stays_under_workspace(&p.mount_name)?;
    // Trimmed: a host path of spaces is not a folder, and `classify_mount_source`
    // is deliberately silent about a path with nothing in it — this is the
    // message that names the mount it belongs to.
    if p.host_path.trim().is_empty() {
        return Err(format!(
            "Folder mounted at '/workspace/{}' has no host path.",
            p.mount_name
        ));
    }
    match classify_mount_source(&p.host_path) {
        None => {}
        Some(UnmountableHostPath::FilesystemRoot { resolved }) => {
            return Err(filesystem_root_message(
                &p.host_path,
                &resolved,
                "using it as a project folder",
            ));
        }
        Some(UnmountableHostPath::NotAbsolute) => {
            return Err(format!(
                "'{}' is not a full path to a folder — where it lands is decided by wherever \
                 Triple-C is running from rather than by you. Give the whole path.",
                p.host_path
            ));
        }
    }
    Ok(())
}

/// The one rule that survives every exemption: the mount has to land under
/// `/workspace`.
///
/// A name carrying a path separator, or one that is nothing but dots, does not
/// name a folder inside `/workspace` — it moves the mount. `/workspace/..` is
/// `/`, `/workspace/../tmp/claude-x` normalises to `/tmp/claude-x`, and
/// `/workspace/.` is `/workspace` itself, shadowing every other mount. The
/// first of those is the C1 chain: a host directory mounted over a path the
/// pre-commit scrub empties as root.
///
/// Everything else `validate_one_path` checks is hygiene — a space or an `@` in
/// a mount name is untidy, not an escape — which is why only this one is
/// applied to rows [`validate_project_paths_update`] otherwise grandfathers.
fn check_mount_name_stays_under_workspace(mount_name: &str) -> Result<(), String> {
    if mount_name.contains('/') || mount_name.contains('\\') {
        return Err(format!(
            "Mount name '{}' contains a path separator, so the folder would be mounted somewhere \
             /workspace/{{name}} does not reach. Use a plain folder name.",
            mount_name
        ));
    }
    // **An empty name is not refused here, and that is a live residual.** It
    // makes the target `/workspace/`, which the daemon normalises to
    // `/workspace` — so the host folder shadows the directory the other mounts
    // land in, and Docker then creates their mount points *inside it*, on the
    // host. It is not refused because it cannot be: an empty name is what a
    // half-filled row holds, `legacy_rows` shows those are already in
    // `projects.json`, and this function runs on grandfathered rows too, so
    // refusing it would make every such project unsavable — the exact
    // regression [`validate_project_paths_update`] exists to prevent.
    //
    // Introducing one is closed at both ends: `validate_one_path` refuses an
    // empty name on any new or edited row, and `WorkspaceSection` no longer
    // sends half-filled or blank ones. What remains is the *stored* row, and it
    // belongs where the mount is built — `docker::create_container` should skip
    // a row with no mount name or no host path, which is the same filter that
    // stops a stored blank row failing the create with
    // `field Source must not be empty`.
    if !mount_name.is_empty() && mount_name.chars().all(|c| c == '.') {
        return Err(format!(
            "Mount name '{}' is not a folder name — it names the directory the mount would sit in.",
            mount_name
        ));
    }
    Ok(())
}

/// Validate the folder list of a project that **already exists**, admitting the
/// rows it is already stored with.
///
/// ## Why this is not just [`validate_project_paths`]
///
/// `update_project` validated nothing at all until recently, while
/// `WorkspaceSection` saved `{paths}` on every blur — so `projects.json` files
/// in the field hold rows that the full rule set refuses: a half-filled row, a
/// mount name with a space in it, two rows sharing a name, `/` as a host path.
///
/// Running the full check on every save made every such project **entirely
/// unsavable**. Not just its folder list: `update_project` is the one command
/// behind the Config tab, so every toggle, every permission-mode change and
/// `useTerminal.ts`'s tab rename came back with a message about folders. The
/// remedy exists — fix the row in the Workspace section — but nothing about
/// "cannot save" on a sandbox switch points at it.
///
/// And blocking the save bought nothing for the rows it was blocking. They are
/// *already stored*, and already mounted on every container start; refusing to
/// persist an unrelated field does not unmount them.
///
/// So: a row carried over verbatim from what is stored is admitted, and a row
/// that is new or edited is held to every rule. That is enough to keep the
/// escalation closed, because escalation means *introducing* a bad value
/// through this command, and an introduced row is never a carried-over one.
///
/// The single exception is [`check_mount_name_stays_under_workspace`], which
/// runs on every row either way. A stored `..` is a live data-loss chain rather
/// than untidy data, its remedy is one edit in the Workspace section, and the
/// message names the mount rather than talking about folders in the abstract.
fn validate_project_paths_update(
    stored: &[ProjectPath],
    incoming: &[ProjectPath],
) -> Result<(), String> {
    let is_blank = |p: &ProjectPath| p.host_path.is_empty() && p.mount_name.is_empty();

    // Rows carried over, counted rather than set-tested: a *second* copy of an
    // existing row is a new row, and has to be checked like one.
    let mut carried: std::collections::HashMap<(&str, &str), usize> =
        std::collections::HashMap::new();
    for p in stored.iter().filter(|p| !is_blank(p)) {
        *carried
            .entry((p.host_path.as_str(), p.mount_name.as_str()))
            .or_insert(0) += 1;
    }

    for p in incoming.iter().filter(|p| !is_blank(p)) {
        check_mount_name_stays_under_workspace(&p.mount_name)?;

        match carried.get_mut(&(p.host_path.as_str(), p.mount_name.as_str())) {
            Some(remaining) if *remaining > 0 => {
                *remaining -= 1;
                log::debug!(
                    "Admitting a stored folder row unchanged: '{}' at /workspace/{}",
                    p.host_path,
                    p.mount_name
                );
            }
            _ => validate_one_path(p)?,
        }
    }

    // Duplicates get the same treatment, one level up: a name may repeat as
    // many times as it already did, and no more. Counting both sides keeps the
    // answer independent of the order the rows arrive in.
    let count_names = |rows: &[ProjectPath]| {
        let mut counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for p in rows.iter().filter(|p| !is_blank(p)) {
            *counts.entry(p.mount_name.clone()).or_insert(0) += 1;
        }
        counts
    };
    let stored_names = count_names(stored);
    for (name, count) in count_names(incoming) {
        let allowed = stored_names.get(&name).copied().unwrap_or(0).max(1);
        if count > allowed {
            return Err(format!("Duplicate mount name '{}'.", name));
        }
    }

    Ok(())
}

/// Refuse a filesystem root newly set as `ssh_key_path` or `ca_cert_path`.
///
/// Both are bind-mounted into the container by `docker::create_container` —
/// `/tmp/.host-ssh` and `/tmp/.host-ca` — and neither had any check at all, so
/// `/` handed the whole host filesystem to the agent to read. Read-only, so
/// this is disclosure rather than the read-write hole a `/` project folder is,
/// but it is the same check: [`classify_mount_source`], resolved rather than
/// spelled, so `/..` and `/home/..` are refused here too.
///
/// Same grandfathering as the folder list, for the same reason: a value already
/// stored is already mounted on every start, and refusing an unrelated save
/// does not unmount it. Only a *change* is held to the rule.
pub(crate) fn validate_mounted_host_path(
    label: &str,
    stored: Option<&str>,
    incoming: Option<&str>,
) -> Result<(), String> {
    let Some(value) = incoming.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(());
    };
    if stored.map(str::trim) == Some(value) {
        return Ok(());
    }
    match classify_mount_source(value) {
        None => {}
        Some(UnmountableHostPath::FilesystemRoot { resolved }) => {
            return Err(filesystem_root_message(
                value,
                &resolved,
                &format!("setting it as {}", label),
            ));
        }
        Some(UnmountableHostPath::NotAbsolute) => {
            return Err(format!(
                "'{}' is not a full path, so it cannot be used as {}. Give the whole path.",
                value, label
            ));
        }
    }
    Ok(())
}

/// Why a host path may not be used as the source of a bind mount.
///
/// Two answers rather than a `bool` because they need different sentences, and
/// because "is a root" is no longer a question about how the path is *spelled*
/// — the refusal has to be able to say where the path actually landed.
#[derive(Debug, PartialEq)]
enum UnmountableHostPath {
    /// The path is, or resolves to, the root of a filesystem. `resolved` is
    /// what it lands on, which is the same string only when a root was typed
    /// outright.
    FilesystemRoot { resolved: String },
    /// The path does not name a location at all. Where it lands is decided by
    /// whatever directory Triple-C happens to be running from, so it can be a
    /// root tomorrow and a folder today, and nothing here can judge it.
    NotAbsolute,
}

/// Length of a `C:` drive prefix at the head of `path`, or 0.
///
/// Duplicated from `commands::file_commands::drive_prefix_len`, together with
/// [`is_windows_style_path`] and [`normalize_host_path`] below. Those are
/// private to that module and it is not this branch's file to change; if the
/// two copies are ever merged, that one is the original and carries the wider
/// test coverage.
fn drive_prefix_len(path: &str) -> usize {
    let b = path.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        2
    } else {
        0
    }
}

/// Whether `path` is written in Windows form, and so whether `\` separates its
/// components. On Linux a backslash is an ordinary filename character, which is
/// why this is a question rather than an unconditional substitution.
///
/// Copy of `file_commands::is_windows_style_path` — see [`drive_prefix_len`].
fn is_windows_style_path(path: &str) -> bool {
    cfg!(windows) || path.starts_with("\\\\") || drive_prefix_len(path) > 0
}

/// `path` with its separators unified and any Win32 verbatim/device prefix
/// removed — the form every rule below is expressed against.
///
/// `\\?\C:\Windows` and `\\?\UNC\server\share` name the *same locations* as
/// `C:\Windows` and `\\server\share`; the prefix only turns off Win32 path
/// parsing. Stripping it is what stops four characters being a bypass — and it
/// has to run on our own output as well, because `std::fs::canonicalize` hands
/// back exactly that spelling on Windows.
///
/// Copy of `file_commands::normalize_host_path` — see [`drive_prefix_len`].
fn normalize_host_path(path: &str) -> String {
    let mut s = if is_windows_style_path(path) {
        path.replace('\\', "/")
    } else {
        path.to_string()
    };
    // Slicing by byte index is safe here only because a prefix matched
    // case-insensitively as ASCII is ASCII, so its end is a char boundary.
    for prefix in ["//?/unc/", "//./unc/"] {
        if s.len() >= prefix.len()
            && s.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        {
            return format!("//{}", &s[prefix.len()..]);
        }
    }
    for prefix in ["//?/", "//./"] {
        if s.len() >= prefix.len()
            && s.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        {
            s = s[prefix.len()..].to_string();
            break;
        }
    }
    s
}

/// A normalised absolute path split into the root it hangs off and the part
/// below it, or `None` when it names no location at all.
///
/// The three roots the desktop platforms have: `/`, a drive (`C:/`), and a UNC
/// share (`//server/share` — the share *is* the root; `//server` alone names a
/// machine and nothing on it).
fn split_host_root(norm: &str) -> Option<(&str, &str)> {
    if let Some(rest) = norm.strip_prefix("//") {
        let mut parts = rest.splitn(3, '/');
        let server = parts.next().unwrap_or("");
        let share = parts.next().unwrap_or("");
        if server.is_empty() || share.is_empty() {
            // `//server`, `//server/`: no share, so nothing under it is named.
            return Some((norm, ""));
        }
        let root_len = 2 + server.len() + 1 + share.len();
        return Some((&norm[..root_len], &norm[root_len..]));
    }
    let drive = drive_prefix_len(norm);
    if drive > 0 {
        // `C:x` is drive-*relative* — it means "x under the current directory
        // on C:", which is a location only the process's own state decides.
        return match norm[drive..].strip_prefix('/') {
            Some(tail) => Some((&norm[..drive + 1], tail)),
            None if norm.len() == drive => Some((norm, "")),
            None => None,
        };
    }
    norm.strip_prefix('/').map(|tail| (&norm[..1], tail))
}

/// How many named components deep `tail` ends up, with `.` dropped and `..`
/// applied — clamped at the root, because `/..` is `/` and not an error.
fn depth_below_root(tail: &str) -> usize {
    let mut depth = 0usize;
    for segment in tail.split('/') {
        match segment {
            "" | "." => {}
            ".." => depth = depth.saturating_sub(1),
            _ => depth += 1,
        }
    }
    depth
}

/// Whether a host path can be handed to Docker as a bind-mount source, and if
/// not, why.
///
/// ## Resolved, not spelled
///
/// This used to be `is_filesystem_root`, and it was purely lexical: trim the
/// trailing separators, say yes to what was left over only if it was empty or a
/// bare `C:`. Nothing in this file called `canonicalize`, so `/..`, `/./`,
/// `/home/..`, `/etc/../` and `C:\..` all sailed through and were passed
/// verbatim to `docker::create_container`, which builds a
/// `Mount { source, read_only: Some(false) }` out of them. Verified against the
/// daemon: `-v /..:/mnt/probe` mounts the host root. That is the whole host
/// filesystem, read-write, in a container whose agent has passwordless sudo —
/// the same escalation `check_mount_name_stays_under_workspace` exists to
/// close, reached through the host-path half of the mount instead of the
/// mount-name half.
///
/// So the answer comes from the OS where the OS can give one: `canonicalize`
/// applies `..`, follows every symlink in the path, and on Windows returns the
/// long name for an 8.3 alias and the verbatim spelling of a UNC share — all
/// things a string comparison cannot see.
///
/// ## When the path cannot be resolved
///
/// `canonicalize` fails on a path that does not exist *here*, which is an
/// ordinary state rather than an attack: `projects.json` syncs between machines
/// and names `C:\Users\jo\code` on a box that has never had a `C:`, and a
/// folder can be created after the project is. Refusing outright would make
/// every such project unsavable, which is the exact failure
/// [`validate_project_paths_update`] exists to avoid — so an unresolvable path
/// falls back to the lexical answer, with `.` and `..` collapsed by
/// [`depth_below_root`] rather than ignored.
///
/// That is a weaker guarantee, not a wrong one, and the gap is bounded: what
/// resolution adds over the lexical rule is symlinks and 8.3 aliases, and both
/// of those are properties of a path that *exists* — precisely the case where
/// `canonicalize` answers. What is left is a path that does not exist at save
/// time and is a symlink to the root by the time the container starts, i.e. the
/// user doing it to themselves after being asked.
///
/// Blocking on the filesystem here is deliberate: this runs on a save, once per
/// row, and is a single `realpath` walk.
fn classify_mount_source(host_path: &str) -> Option<UnmountableHostPath> {
    let raw = host_path.trim();
    if raw.is_empty() {
        // Emptiness is somebody else's error message — see
        // `validate_one_path`, which names the mount it belongs to.
        return None;
    }

    // Nothing but separators, in either spelling: `/`, `//`, `\`, `\\`. Taken
    // first because a lone `\` is a *relative* name on Linux, and reporting a
    // Windows root as "not a full path" would be answering a question the user
    // did not ask.
    if raw.chars().all(|c| c == '/' || c == '\\') {
        return Some(UnmountableHostPath::FilesystemRoot {
            resolved: raw.to_string(),
        });
    }

    // Absoluteness is judged on what the user typed, **before** resolution.
    //
    // `canonicalize` resolves a relative path against Triple-C's own working
    // directory, so it hands back an absolute path and the `NotAbsolute` branch
    // below never fires — it was reachable only when canonicalize *failed*,
    // i.e. only for relative paths that happened not to exist. That made the
    // verdict depend on where the app was launched from: `.` and `..` were
    // accepted from the repo, refused from `/`. The daemon then refuses the
    // mount outright (`invalid mount path: '..' mount path must be absolute`),
    // so the project saved cleanly and could never start again — the bricking
    // mode `project_path_mounts`'s filter exists to prevent, reached through
    // the host-path half of the row instead of the mount-name half.
    if split_host_root(&normalize_host_path(raw)).is_none() {
        return Some(UnmountableHostPath::NotAbsolute);
    }

    let canonical = std::fs::canonicalize(raw)
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let judged = canonical.as_deref().unwrap_or(raw);
    let norm = normalize_host_path(judged);

    let Some((root, tail)) = split_host_root(&norm) else {
        return Some(UnmountableHostPath::NotAbsolute);
    };
    if depth_below_root(tail) == 0 {
        return Some(UnmountableHostPath::FilesystemRoot {
            resolved: root.to_string(),
        });
    }
    None
}

/// The refusal for a host path that lands on a filesystem root, naming the
/// resolved location as well as what was typed when those differ. `/..` reads
/// as a folder; `/..` *is* `/`, and a message that only quoted it back would
/// leave the user with nothing to act on.
fn filesystem_root_message(typed: &str, resolved: &str, use_for: &str) -> String {
    let where_it_lands = if typed.trim() == resolved {
        format!("'{}' is a filesystem root", typed)
    } else {
        format!("'{}' resolves to '{}', which is a filesystem root", typed, resolved)
    };
    format!(
        "{}, so {} would mount the whole drive into the container. Choose the folder itself.",
        where_it_lands, use_for
    )
}

#[tauri::command]
pub async fn list_projects(state: State<'_, AppState>) -> Result<Vec<Project>, String> {
    Ok(state.projects_store.list())
}

#[tauri::command]
pub async fn add_project(
    name: String,
    paths: Vec<ProjectPath>,
    state: State<'_, AppState>,
) -> Result<Project, String> {
    // Validate paths
    // A new project needs a folder; the blank row `validate_project_paths`
    // tolerates is the Config tab's placeholder on an *existing* one.
    if paths.is_empty()
        || paths
            .iter()
            .all(|p| p.host_path.is_empty() && p.mount_name.is_empty())
    {
        return Err("At least one folder path is required.".to_string());
    }
    validate_project_paths(&paths)?;
    let project = Project::new(name, paths);
    // Nothing can have been blanked on a project that did not exist a line ago.
    store_secrets_for_project(&project, &[])?;
    state.projects_store.add(project)
}

#[tauri::command]
pub async fn remove_project(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<ProjectRemovalReport, String> {
    // **H-2: the only writer of these three categories that held nothing.**
    // This purges migration artifacts, removes `triple-c-snapshot-{id}` and
    // both named volumes — and a compaction resolves that same tag when its
    // build starts and commits back over it minutes later. Compact a project,
    // then remove it from the sidebar, and `restore_image_config` commits a
    // flat image back onto `triple-c-snapshot-{id}:latest` for a project that
    // no longer exists: not dangling, not a `:pre-migration-*` tag, and not
    // reachable by the per-project scan, so no reclaim path can ever see it
    // again. Taken for the whole removal, like every other writer.
    //
    // Refusing is safe for the UI: `useProjects.remove` only drops the sidebar
    // row *after* the command resolves, and `ProjectHome` turns the rejection
    // into a toast, so the row stays and the message names what is running.
    let _guard =
        crate::project_lock::try_acquire(&project_id, crate::project_lock::ProjectOp::Destroy)?;

    // Release any host loopback ports the auth bridge holds for this project
    // before the container (and the project record) go away.
    state.auth_bridge.stop(&project_id).await;

    // A migration record outliving its project leaks a state file, a staged
    // payload tar that can run to several GB, and a `:pre-migration-<ts>` tag
    // holding an entire snapshot image that nothing will ever reference again.
    crate::commands::migration_commands::purge_migration_artifacts(&project_id).await;

    // Stop and remove container if it exists. Everything named in `report`
    // below is what will be unreachable the moment this function drops the
    // project record — see [`ProjectRemovalReport`] and
    // `storage::pending_cleanup`, which is what makes it reachable anyway.
    let mut report = ProjectRemovalReport::default();
    let existing_project = state.projects_store.get(&project_id);

    if let Some(ref project) = existing_project {
        // `project.container_id` can be `None` or stale — a crash between
        // creating a container and persisting its id is the same race every
        // other destroyer of a project's container already guards against
        // with `find_existing_container` (`start_project_container`,
        // migration's recreate paths). Removal is the one place that
        // mattered least before this fix, because a container `remove_project`
        // missed just sat there; now a miss here poisons the volume removal
        // right after it (Docker refuses to delete a volume a container still
        // references) and mints a pending-cleanup record for volumes with no
        // way to name the container actually blocking them.
        let container_ref = match &project.container_id {
            Some(id) => Some(id.clone()),
            None => docker::find_existing_container(project).await.ok().flatten(),
        };
        if let Some(ref container_id) = container_ref {
            state.exec_manager.close_sessions_for_container(container_id).await;
            let _ = docker::stop_container(container_id).await;
            if let Err(e) = docker::remove_container(container_id).await {
                log::warn!(
                    "Failed to remove container {} for project {}: {}",
                    container_id, project_id, e
                );
                // Recorded by name, not id: the name is the stable handle a
                // later retry can still resolve (Docker's remove-container
                // call accepts either), and it is what `container_ref` above
                // falls back to finding in the first place.
                report.container = Some(project.container_name());
            }
        }

        // Legacy MCP cleanup (pre-MCP-removal installs): drop any leftover MCP
        // containers first, then the per-project network they were attached to.
        docker::remove_legacy_mcp_containers(&project.id).await;
        docker::remove_legacy_project_network(&project.id).await;

        // Clean up the snapshot image + volumes
        if let Err(e) = docker::remove_snapshot_image(project).await {
            log::warn!("Failed to remove snapshot image for project {}: {}", project_id, e);
            report.image = Some(docker::get_snapshot_image_name(project));
        }
        report.volumes = docker::remove_project_volumes(project).await;
    }

    // Clean up keychain secrets for this project
    if let Err(e) = secure::delete_project_secrets(&project_id) {
        log::warn!("Failed to delete keychain secrets for project {}: {}", project_id, e);
    }

    if !report.is_clean() {
        let record = crate::storage::pending_cleanup::PendingCleanup {
            project_id: project_id.clone(),
            project_name: existing_project.map(|p| p.name).unwrap_or_default(),
            container_id: report.container.clone(),
            image: report.image.clone(),
            volumes: report.volumes.clone(),
            recorded_at: chrono::Utc::now().to_rfc3339(),
        };
        match crate::storage::pending_cleanup::save(&record) {
            Ok(()) => {
                report.retry_scheduled = true;
                log::warn!(
                    "Project {} removed with Docker resources still present: {:?} — recorded for \
                     automatic retry on next launch",
                    project_id, report
                );
            }
            Err(e) => {
                report.retry_scheduled = false;
                log::error!(
                    "Project {} removed with Docker resources still present ({:?}), and the \
                     pending-cleanup record could not be written ({}) — nothing will retry \
                     removing them",
                    project_id, report, e
                );
            }
        }
    }

    state.projects_store.remove(&project_id)?;
    Ok(report)
}

/// Retry every pending-cleanup record left behind by a [`remove_project`]
/// that could not finish. Run once at startup alongside the other reapers
/// (see `lib.rs`'s "Startup disk housekeeping" block) — never on a timer and
/// never blocking anything, since a locked volume or an in-use image can sit
/// unresolved for an arbitrary amount of time and the daemon may not even be
/// up yet.
///
/// Not a `#[tauri::command]`: nothing in the UI surfaces this list yet
/// (deliberately — see `SnapshotSweepReport`'s doc comment for the same
/// reasoning), so there is no IPC contract to keep. A record that still has
/// leftovers after this is written back so the next run does not lose track
/// of what changed; one that is now empty is deleted.
pub async fn retry_pending_cleanup_logged() {
    let records = crate::storage::pending_cleanup::list();
    if records.is_empty() {
        return;
    }

    let mut cleaned = 0usize;
    let mut still_pending = 0usize;

    for mut record in records {
        if let Some(container_id) = record.container_id.take() {
            match docker::remove_container(&container_id).await {
                Ok(()) => {}
                Err(e) => {
                    log::warn!(
                        "Pending cleanup: still could not remove container {} for project {} \
                         ({}): {}",
                        container_id, record.project_id, record.project_name, e
                    );
                    record.container_id = Some(container_id);
                }
            }
        }

        if let Some(image) = record.image.take() {
            match docker::remove_image_by_name(&image).await {
                Ok(()) => {}
                Err(e) => {
                    log::warn!(
                        "Pending cleanup: still could not remove image {} for project {} ({}): {}",
                        image, record.project_id, record.project_name, e
                    );
                    record.image = Some(image);
                }
            }
        }

        if !record.volumes.is_empty() {
            record.volumes = docker::remove_volumes_by_name(&record.volumes).await;
        }

        if record.is_empty() {
            if let Err(e) = crate::storage::pending_cleanup::clear(&record.project_id) {
                log::warn!(
                    "Pending cleanup for project {} ({}) finished but the record could not be \
                     deleted: {}",
                    record.project_id, record.project_name, e
                );
            }
            cleaned += 1;
        } else {
            still_pending += 1;
            // `recorded_at` is otherwise write-only — nothing read it back,
            // which is exactly the shape `storage::migration_store` calls out
            // as a bug in its own history ("nothing ever removed them"). A
            // record that has failed every retry for a week is no longer
            // routine: escalate the log level so it is not indistinguishable
            // from one seen for the first time.
            let age = chrono::DateTime::parse_from_rfc3339(&record.recorded_at)
                .ok()
                .map(|t| chrono::Utc::now().signed_duration_since(t.with_timezone(&chrono::Utc)));
            match age {
                Some(age) if age > chrono::Duration::days(PENDING_CLEANUP_STALE_AFTER_DAYS) => {
                    log::error!(
                        "Pending cleanup for project {} ({}) has not succeeded in over {} days: \
                         {:?} — this may need a manual `docker volume rm` / `docker rmi` / \
                         `docker rm`",
                        record.project_id, record.project_name, PENDING_CLEANUP_STALE_AFTER_DAYS, record
                    );
                }
                _ => {}
            }
            if let Err(e) = crate::storage::pending_cleanup::save(&record) {
                log::warn!(
                    "Could not update pending cleanup record for project {} ({}): {}",
                    record.project_id, record.project_name, e
                );
            }
        }
    }

    log::info!(
        "Pending cleanup retry: {} project(s) fully cleaned up, {} still have leftovers",
        cleaned, still_pending
    );
}

/// After this many days of a pending-cleanup record failing every retry,
/// `retry_pending_cleanup_logged` escalates its log line from `warn` to
/// `error` — see the comment at its call site.
const PENDING_CLEANUP_STALE_AFTER_DAYS: i64 = 7;

#[tauri::command]
pub async fn update_project(
    project: serde_json::Value,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Project, String> {
    // Taken as raw JSON, then deserialised, for one reason: a secret field that
    // arrived as `null` is a credential the user cleared, and a secret field
    // that did not arrive at all belongs to whichever editor is not saving
    // right now. `Option<String>` cannot tell those apart — see
    // [`explicitly_cleared_secrets`].
    let explicitly_cleared = explicitly_cleared_secrets(&project);
    let mut project: Project = serde_json::from_value(project)
        .map_err(|e| format!("Could not read the project being saved: {}", e))?;

    // Fields this command does not get to write, whoever is calling it.
    //
    // `container_id` is the one that matters: it is the handle the whole file
    // command surface resolves against, `list_sibling_containers` used to hand the
    // webview the ids of every other container on the daemon, and a project
    // save is not the place a container is adopted. It is assigned by
    // `start_project_container` through `projects_store::set_container_id` and
    // read back here. `status` has its own setter (`update_status`) for the
    // same reason, and neither `id` nor `created_at` is a thing a save can
    // mean to change.
    //
    // The privileged *toggles* — `allow_docker_access`, `vpn_support_enabled`,
    // `sandbox_mode_enabled`, `permission_mode` — are deliberately not in this
    // list: they are what the Config tab's own switches write, through this
    // command, and there is nothing here that can tell that call apart from
    // any other. Their boundary is the webview, not this function.
    let stored = state
        .projects_store
        .get(&project.id)
        .ok_or_else(|| format!("Project {} not found", project.id))?;

    // **This takes a whole `Project` over IPC and used to store it verbatim.**
    // `add_project` validated its folder list and this did not, so every check
    // there was one edit away from being bypassed — and the Config tab's mount
    // name is a free-text field on an existing project, saved on blur, calling
    // exactly this command. See [`validate_project_paths`] for what a mount
    // name of `../tmp/claude-x` does to the user's files.
    //
    // Validated against what is *stored*, not in isolation: a rule this command
    // never enforced can be violated by data already on disk, and a project
    // that cannot be saved at all is a Config tab that cannot be used at all.
    // See [`validate_project_paths_update`].
    validate_project_paths_update(&stored.paths, &project.paths)?;
    validate_mounted_host_path(
        "the SSH key folder",
        stored.ssh_key_path.as_deref(),
        project.ssh_key_path.as_deref(),
    )?;
    validate_mounted_host_path(
        "the CA certificate path",
        stored.ca_cert_path.as_deref(),
        project.ca_cert_path.as_deref(),
    )?;

    // Custom env var names had no charset check anywhere, so a key like
    // `BASH_FUNC_stat%%` reached the container environment verbatim. Same
    // grandfathering as the folder list, for the same reason — see
    // [`crate::models::validate_env_vars_update`].
    crate::models::validate_env_vars_update(&stored.custom_env_vars, &project.custom_env_vars)?;

    project.container_id = stored.container_id;
    project.status = stored.status;
    project.created_at = stored.created_at;
    project.updated_at = chrono::Utc::now().to_rfc3339();

    store_secrets_for_project(&project, &explicitly_cleared)?;
    let updated = state.projects_store.update(project)?;

    // `auth_bridge_enabled` can arrive through this generic save as well as
    // through `set_auth_bridge_enabled`, so reconcile the running bridge with
    // whatever was just persisted. `start` is idempotent and `stop` is a no-op
    // when nothing is running, so this is safe on every project save.
    if updated.auth_bridge_enabled {
        if let Some(ref container_id) = updated.container_id {
            if docker::is_container_running(container_id).await.unwrap_or(false) {
                state
                    .auth_bridge
                    .start(
                        updated.id.clone(),
                        container_id.clone(),
                        app_handle,
                        state.projects_store.clone(),
                    )
                    .await;
            }
        }
    } else {
        state.auth_bridge.stop(&updated.id).await;
    }

    Ok(updated)
}

#[tauri::command]
pub async fn start_project_container(
    project_id: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Project, String> {
    // **Acquired, not polled.** A migration removes the container and creates
    // its replacement moments later. Starting in that window finds no
    // container, creates a second one under the same name, and the migration's
    // own create then fails on the name conflict — which sends it into an
    // auto-rollback that also cannot create. This used to be a one-shot
    // `is_migrating` read, which covered that case and no other: a start also
    // commits `triple-c-snapshot-{id}:latest`, so it races a *compaction*
    // committing the same tag with nothing between them. The claim is held for
    // the whole start rather than checked at its door.
    //
    // The UI already refuses (`canMigrate` gates on the container being stopped
    // and no run being in flight); this is the same gate on the side that
    // actually owns the invariant.
    let _guard =
        crate::project_lock::try_acquire(&project_id, crate::project_lock::ProjectOp::Recreate)?;
    start_project_container_locked(project_id, app_handle, state).await
}

/// The body of [`start_project_container`], with **no claim of its own**.
///
/// Split out for exactly one caller: [`rebuild_project_container`] already
/// holds the project under [`crate::project_lock::ProjectOp::Reset`] for its
/// whole run, and a Reset that then went through the public command would be
/// refused by its own guard. Every other path must go through the command.
async fn start_project_container_locked(
    project_id: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Project, String> {
    let mut project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    // Populate secret fields from the OS keychain so they are available
    // in memory when building environment variables for the container.
    load_secrets_for_project(&mut project);

    // Load settings for image resolution and global AWS
    let settings = state.settings_store.get();
    let image_name = container_config::resolve_image_name(&settings.image_source, &settings.custom_image_name);

    // Validate backend requirements
    if project.backend == Backend::Bedrock {
        let bedrock = project.bedrock_config.as_ref()
            .ok_or_else(|| "Bedrock backend selected but no Bedrock configuration found.".to_string())?;
        // Region can come from per-project or global
        if bedrock.aws_region.is_empty() && settings.global_aws.aws_region.is_none() {
            return Err("AWS region is required for Bedrock backend. Set it per-project or in global AWS settings.".to_string());
        }
    }

    if project.backend == Backend::Ollama {
        let ollama = project.ollama_config.as_ref()
            .ok_or_else(|| "Ollama backend selected but no Ollama configuration found.".to_string())?;
        if ollama.base_url.is_empty()
            && settings.global_ollama.base_url.as_deref().map(str::trim).unwrap_or("").is_empty()
        {
            return Err("Ollama base URL is required. Set it per-project or in global Ollama settings.".to_string());
        }
    }

    if project.backend == Backend::LlamaCpp {
        let cfg = project.llamacpp_config.as_ref()
            .ok_or_else(|| "llama.cpp backend selected but no llama.cpp configuration found.".to_string())?;
        if cfg.base_url.trim().is_empty()
            && settings.global_llamacpp.base_url.as_deref().map(str::trim).unwrap_or("").is_empty()
        {
            return Err("llama.cpp base URL is required. Set it per-project or in global llama.cpp settings.".to_string());
        }
    }

    if project.backend == Backend::OpenAiCompatible {
        let oai_config = project.openai_compatible_config.as_ref()
            .ok_or_else(|| "OpenAI Compatible backend selected but no configuration found.".to_string())?;
        if oai_config.base_url.is_empty()
            && settings.global_openai_compatible.base_url.as_deref().map(str::trim).unwrap_or("").is_empty()
        {
            return Err("OpenAI Compatible base URL is required. Set it per-project or in global settings.".to_string());
        }
    }

    // Update status to starting
    state.projects_store.update_status(&project_id, ProjectStatus::Starting)?;

    // Pre-validate AWS SSO session on the host for Bedrock Profile projects.
    // If the session is expired, trigger `aws sso login` before starting the container
    // so the entrypoint copies already-fresh credentials from the host mount.
    if project.backend == Backend::Bedrock {
        if let Some(ref bedrock) = project.bedrock_config {
            if bedrock.auth_method == BedrockAuthMethod::Profile {
                let profile = aws_commands::resolve_profile_for_project(
                    &project,
                    settings.global_aws.aws_profile.as_deref(),
                );

                emit_progress(&app_handle, &project_id, "Validating AWS session...");

                let session_valid = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    aws_commands::check_sso_session(&profile),
                )
                .await;

                match session_valid {
                    Ok(Ok(true)) => {
                        emit_progress(&app_handle, &project_id, "AWS session valid.");
                    }
                    Ok(Ok(false)) => {
                        // Session expired — check if this is an SSO profile
                        if aws_commands::is_sso_profile(&profile).await.unwrap_or(false) {
                            emit_progress(
                                &app_handle,
                                &project_id,
                                "AWS session expired. Starting SSO login (check your browser)...",
                            );
                            match aws_commands::run_sso_login(&profile).await {
                                Ok(()) => {
                                    emit_progress(
                                        &app_handle,
                                        &project_id,
                                        "SSO login successful.",
                                    );
                                }
                                Err(e) => {
                                    log::warn!(
                                        "SSO login failed for profile '{}': {} — continuing anyway",
                                        profile,
                                        e
                                    );
                                    emit_progress(
                                        &app_handle,
                                        &project_id,
                                        "SSO login failed or cancelled. Continuing...",
                                    );
                                }
                            }
                        } else {
                            log::warn!(
                                "AWS session invalid for profile '{}' (not SSO). Continuing...",
                                profile
                            );
                        }
                    }
                    Ok(Err(e)) => {
                        log::warn!("Failed to check AWS session: {} — continuing anyway", e);
                    }
                    Err(_) => {
                        log::warn!("AWS session check timed out — continuing anyway");
                    }
                }
            }
        }
    }

    // Wrap container operations so that any failure resets status to Stopped.
    let result: Result<String, String> = async {
        // Ensure image exists
        emit_progress(&app_handle, &project_id, "Checking image...");
        if !docker::image_exists(&image_name).await? {
            return Err(format!("Docker image '{}' not found. Please pull or build the image first.", image_name));
        }

        // Determine docker socket path
        let docker_socket = settings.docker_socket_path
            .as_deref()
            .map(|s| s.to_string())
            .unwrap_or_else(|| default_docker_socket());

        // AWS config path from global settings
        let aws_config_path = settings.global_aws.aws_config_path.clone();

        // What we would create this container from *right now*: the project's
        // snapshot when one exists, else the configured base. This is the value
        // `container_needs_recreation` compares against the container's
        // `triple-c.create-image` label — the check that replaced the old
        // tautological one. It is resolved *before* the commit below, so it
        // describes the pre-commit world the existing container was born into.
        let snapshot_image = docker::get_snapshot_image_name(&project);
        let expected_create_image =
            if docker::image_exists(&snapshot_image).await.unwrap_or(false) {
                snapshot_image.clone()
            } else {
                image_name.clone()
            };

        let container_id = if let Some(existing_id) = docker::find_existing_container(&project).await? {
            // Check if config changed — if so, snapshot + recreate
            let needs_recreate = docker::container_needs_recreation(
                &existing_id,
                &project,
                &expected_create_image,
                &settings.global_aws,
                &settings.global_ollama,
                &settings.global_llamacpp,
                &settings.global_openai_compatible,
                settings.global_claude_instructions.as_deref(),
                &settings.global_custom_env_vars,
                settings.timezone.as_deref(),
                settings.global_claude_code_settings.as_ref(),
                settings.default_ssh_key_path.as_deref(),
                settings.ca_cert_path.as_deref(),
                settings.default_git_user_name.as_deref(),
                settings.default_git_user_email.as_deref(),
            ).await.unwrap_or(false);

            if needs_recreate {
                log::info!("Container config changed for project {} — committing snapshot and recreating", project.id);
                // Snapshot the filesystem before destroying
                emit_progress(&app_handle, &project_id, "Saving container state...");
                if let Err(e) = docker::commit_container_snapshot(&existing_id, &project).await {
                    log::warn!("Failed to snapshot container before recreation: {}", e);
                }
                emit_progress(&app_handle, &project_id, "Recreating container...");
                let _ = docker::stop_container(&existing_id).await;
                docker::remove_container(&existing_id).await?;

                // Legacy MCP cleanup: the old container may have been attached to
                // `triple-c-net-<projectId>`. Tear down leftover MCP containers and
                // that network now, before the replacement is created without it.
                docker::remove_legacy_mcp_containers(&project.id).await;
                docker::remove_legacy_project_network(&project.id).await;

                // Create from snapshot image (preserves system-level changes).
                // Re-resolved after the commit above: when no snapshot existed
                // before, one does now, and creating from the base instead
                // would throw away the state that was just saved.
                let create_image = if docker::image_exists(&snapshot_image).await.unwrap_or(false) {
                    snapshot_image.clone()
                } else {
                    image_name.clone()
                };

                let new_id = create_container_for_project(
                    &project,
                    &settings,
                    &docker_socket,
                    aws_config_path.as_deref(),
                    &create_image,
                    &image_name,
                    docker::CreateExtras::default(),
                ).await?;
                emit_progress(&app_handle, &project_id, "Starting container...");
                docker::start_container(&new_id).await?;

                // The commit above moved `:latest` and orphaned the image it
                // used to point at; the container holding that image open was
                // removed a few lines up, so now is when Docker will actually
                // let it go. Detached because this is housekeeping and the
                // project is already running — and it sweeps every orphan, not
                // just this one, so recreations that happened before the sweep
                // existed are cleaned up too.
                tauri::async_runtime::spawn(async {
                    docker::sweep_orphaned_snapshots_logged("after recreation").await;
                });

                new_id
            } else {
                emit_progress(&app_handle, &project_id, "Starting container...");
                docker::start_container(&existing_id).await?;
                existing_id
            }
        } else {
            // Container doesn't exist (first start, or Docker pruned it).
            // Check for a snapshot image first — it preserves system-level
            // changes (apt/pip/npm installs) from the previous session.
            if expected_create_image == snapshot_image {
                log::info!("Creating container from snapshot image for project {}", project.id);
            }
            let create_image = expected_create_image.clone();

            emit_progress(&app_handle, &project_id, "Creating container...");
            let new_id = create_container_for_project(
                &project,
                &settings,
                &docker_socket,
                aws_config_path.as_deref(),
                &create_image,
                &image_name,
                docker::CreateExtras::default(),
            ).await?;
            emit_progress(&app_handle, &project_id, "Starting container...");
            docker::start_container(&new_id).await?;
            new_id
        };

        // Sync Bedrock credentials on every start: refresh static/session creds
        // so rotated keys are picked up without a full container recreation, and
        // clear stale creds when the project no longer uses static-cred Bedrock.
        if let Err(e) = docker::sync_bedrock_credentials(&container_id, &project).await {
            log::warn!("Failed to sync AWS credentials for project {}: {}", project.id, e);
        }

        Ok(container_id)
    }.await;

    // On failure, reset status to Stopped so the project doesn't get stuck.
    if let Err(ref e) = result {
        log::error!("Failed to start container for project {}: {}", project_id, e);
        let _ = state.projects_store.update_status(&project_id, ProjectStatus::Stopped);
    }

    let container_id = result?;

    // Update project with container info using granular methods (Issue 14: TOCTOU)
    state.projects_store.set_container_id(&project_id, Some(container_id.clone()))?;
    state.projects_store.update_status(&project_id, ProjectStatus::Running)?;

    // Arm the auth bridge if this project opted in. Purely host-side, so it
    // happens after the container is up and never affects the start itself.
    if project.auth_bridge_enabled {
        state
            .auth_bridge
            .start(
                project_id.clone(),
                container_id.clone(),
                app_handle.clone(),
                state.projects_store.clone(),
            )
            .await;
    }

    project.container_id = Some(container_id);
    project.status = ProjectStatus::Running;
    Ok(project)
}

#[tauri::command]
pub async fn stop_project_container(
    project_id: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Stop had **no** exclusion at all, which is the one gap CLAUDE.md's "every
    // command that stops, removes or recreates the container consults
    // `is_migrating`" rule already named and this function did not honour. A
    // stop lands on the container a migration is mid-swap on, and it closes the
    // exec sessions a compaction's config replay is not expecting to lose.
    let _guard =
        crate::project_lock::try_acquire(&project_id, crate::project_lock::ProjectOp::Recreate)?;

    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    state.projects_store.update_status(&project_id, ProjectStatus::Stopping)?;

    // Drop host listeners first: they only make sense while the container runs.
    state.auth_bridge.stop(&project_id).await;

    if let Some(ref container_id) = project.container_id {
        // Close exec sessions for this project
        emit_progress(&app_handle, &project_id, "Stopping container...");
        state.exec_manager.close_sessions_for_container(container_id).await;

        if let Err(e) = docker::stop_container(container_id).await {
            log::warn!("Docker stop failed for container {} (project {}): {} — resetting to Stopped anyway", container_id, project_id, e);
        }
    }

    state.projects_store.update_status(&project_id, ProjectStatus::Stopped)?;
    Ok(())
}

#[tauri::command]
pub async fn rebuild_project_container(
    project_id: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<ProjectResetOutcome, String> {
    // Reset deletes both volumes and the snapshot image. Doing that while a
    // migration is mid-flight pulls the ground out from under it and leaves an
    // orphan migration record pointing at images that no longer exist — and
    // doing it while a *compaction* is mid-flight is worse, because the
    // compaction then commits `flat(old)` back over the `:latest` this just
    // destroyed and resurrects the system layer the user asked to be rid of.
    // Held for the whole Reset, including the start at the end of it.
    let _guard =
        crate::project_lock::try_acquire(&project_id, crate::project_lock::ProjectOp::Reset)?;

    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    // Reset supersedes any migration decision that was still pending: the
    // snapshot image and both volumes are about to go, so a surviving record
    // could only describe things that no longer exist — while its
    // `:pre-migration-<ts>` tag held a whole snapshot image (multiple GB) alive
    // with nothing left that could ever use it.
    crate::commands::migration_commands::purge_migration_artifacts(&project_id).await;

    // The bridge is bound to the container that is about to be destroyed;
    // `start_project_container` below re-arms it against the new one.
    state.auth_bridge.stop(&project_id).await;

    // Remove existing container. Resolved the same way `remove_project` now
    // is — `project.container_id` can be `None` or stale — because a
    // container this misses blocks the volume removal immediately below with
    // a 409, and Reset silently keeping the old volumes is exactly the bug
    // this whole change is closing.
    let container_ref = match &project.container_id {
        Some(id) => Some(id.clone()),
        None => docker::find_existing_container(&project).await.ok().flatten(),
    };
    if let Some(ref container_id) = container_ref {
        state.exec_manager.close_sessions_for_container(container_id).await;
        let _ = docker::stop_container(container_id).await;
        docker::remove_container(container_id).await?;
        state.projects_store.set_container_id(&project_id, None)?;
    }

    // Remove snapshot image + volumes so Reset creates from the clean base image
    if let Err(e) = docker::remove_snapshot_image(&project).await {
        log::warn!("Failed to remove snapshot image for project {}: {}", project_id, e);
    }
    let leftover_volumes = docker::remove_project_volumes(&project).await;
    if !leftover_volumes.is_empty() {
        // Unlike `remove_project`, Reset keeps the project record — but a
        // volume that survives this is reused as-is by the container
        // `start_project_container_locked` creates below, which is exactly
        // what Reset promises not to do. No pending-cleanup record: the
        // project id is still live, so a later Reset attempt can retry this
        // itself rather than needing startup housekeeping to do it.
        log::warn!(
            "Reset could not remove volume(s) {:?} for project {} — the new container may reuse \
             their old contents instead of starting clean",
            leftover_volumes, project_id
        );
    }

    // Start fresh. The locked variant, because `_guard` above is this project's
    // claim and the public command would be refused by it.
    let project = start_project_container_locked(project_id, app_handle, state).await?;
    Ok(ProjectResetOutcome { project, leftover_volumes })
}

/// Reconcile project statuses against actual Docker container state.
/// Called by the frontend after Docker is confirmed available. Projects
/// marked as Running whose containers are no longer running get reset
/// to Stopped.
///
/// This is also where an interrupted **base-image migration** is picked up.
/// It runs at startup, which is exactly when a migration that died with the app
/// needs to be noticed — see
/// [`crate::commands::migration_commands::reconcile_migration`]. The migration
/// pass runs over *every* project, not just the Running ones, because a project
/// whose container was removed mid-migration reports Stopped.
#[tauri::command]
pub async fn reconcile_project_statuses(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<Project>, String> {
    let projects = state.projects_store.list();

    for project in &projects {
        crate::commands::migration_commands::reconcile_migration(project, &app_handle).await;
    }

    for project in &projects {
        // `Starting` and `Stopping` are in here as a backstop, not because
        // anything is expected to leave a project in one. They are transitional
        // states owned by an in-flight command, so a project still wearing one
        // is a project whose command died — a crash mid-start, or a migration
        // that bailed out between the stop and the swap. Skipping them, as this
        // loop used to, meant nothing in the app ever put such a project right:
        // it sat at "Stopping" with the Start button disabled, permanently.
        // Docker is the authority either way, so the check below is correct for
        // all four.
        if !matches!(
            project.status,
            ProjectStatus::Running
                | ProjectStatus::Error
                | ProjectStatus::Starting
                | ProjectStatus::Stopping
        ) {
            continue;
        }
        // ...but never for a project this process is actively working on. A
        // migration's container is legitimately absent between the
        // `remove_container` and the create that follows, and so is a Reset's,
        // and so is a start's before its create returns. Reconciling into any
        // of those windows writes `Stopped` over a project that is mid-run.
        // Broadened from `is_migrating` to the whole lock for exactly that
        // reason — the migration was never the only operation with a gap.
        if crate::project_lock::held(&project.id).is_some() {
            continue;
        }

        let is_running = if let Some(ref container_id) = project.container_id {
            docker::is_container_running(container_id).await.unwrap_or(false)
        } else {
            false
        };

        if is_running {
            log::info!(
                "Project '{}' ({}) container is still running — keeping Running status",
                project.name,
                project.id
            );
            // The app may have restarted while the container kept running; the
            // bridge lives in this process, so re-arm it here. `start` is
            // idempotent, so a bridge that is already polling is untouched.
            if project.auth_bridge_enabled {
                if let Some(ref container_id) = project.container_id {
                    state
                        .auth_bridge
                        .start(
                            project.id.clone(),
                            container_id.clone(),
                            app_handle.clone(),
                            state.projects_store.clone(),
                        )
                        .await;
                }
            }
        } else {
            log::info!(
                "Project '{}' ({}) container is not running — setting to Stopped",
                project.name,
                project.id
            );
            let _ = state.projects_store.update_status(&project.id, ProjectStatus::Stopped);
        }
    }

    Ok(state.projects_store.list())
}

fn default_docker_socket() -> String {
    if cfg!(target_os = "windows") {
        "//./pipe/docker_engine".to_string()
    } else {
        "/var/run/docker.sock".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(host: &str, mount: &str) -> ProjectPath {
        ProjectPath {
            host_path: host.to_string(),
            mount_name: mount.to_string(),
        }
    }

    /// The mount name that reaches the scrub. `docker::create_container` builds
    /// `/workspace/{mount_name}`, and the daemon normalises `..` out of it —
    /// verified against Engine 29.7, where a mount created with a target of
    /// `/workspace/../tmp/claude-x` is reported by `inspect` as `/tmp/claude-x`.
    #[test]
    fn a_mount_name_cannot_walk_out_of_workspace() {
        for escape in ["..", "../tmp/claude-x", "../../etc", "/tmp/claude-x", "a/../.."] {
            assert!(
                validate_project_paths(&[path("/home/u/project", escape)]).is_err(),
                "mount name '{}' was accepted, which puts the host folder somewhere \
                 /workspace/{{name}} does not reach",
                escape
            );
        }
        // The dotted names that are *not* a traversal stay usable.
        for ok in ["my.project", ".hidden", "a.b-c_d", "workspace2"] {
            assert!(
                validate_project_paths(&[path("/home/u/project", ok)]).is_ok(),
                "mount name '{}' should be usable",
                ok
            );
        }
    }

    #[test]
    fn the_whole_host_filesystem_cannot_be_mounted() {
        // A read-write bind of `/` into a container whose agent has
        // passwordless sudo.
        for root in ["/", "//", "\\", "C:\\", "c:/", "D:"] {
            assert!(
                validate_project_paths(&[path(root, "everything")]).is_err(),
                "host path '{}' was accepted as a project folder",
                root
            );
        }
        assert!(validate_project_paths(&[path("/home/u/project", "project")]).is_ok());
        assert!(validate_project_paths(&[path("C:\\Users\\u\\project", "project")]).is_ok());
    }

    /// Every spelling of a root that is not *spelled* like one.
    ///
    /// The predicate this replaces trimmed trailing separators and compared
    /// what was left, so `/..` — which the daemon mounts as the host root,
    /// verified with `docker run -v /..:/mnt/probe` — was indistinguishable
    /// from a project folder called `..`. No test in the repo contained a `.`
    /// or a `..` in a host path, which is why it shipped.
    #[test]
    fn a_host_path_that_resolves_to_a_root_is_refused() {
        let escapes = [
            "/..",
            "/../",
            "/./",
            "/.",
            "/home/..",
            "/etc/../",
            "/tmp/../..",
            // Deliberately not present on any machine, so this is the
            // unresolvable path taking the lexical route.
            "/no-such-dir-here/../..",
            "C:\\..",
            "C:\\Users\\..",
            "c:/foo/..",
            // Win32 verbatim spelling of a drive root.
            "\\\\?\\C:\\",
            "\\\\?\\C:\\..",
            // A UNC share root is the root of everything on that share, which
            // the old predicate accepted despite its doc comment claiming
            // otherwise.
            "\\\\server\\share",
            "//server/share/",
        ];
        for escape in escapes {
            assert!(
                validate_project_paths(&[path(escape, "everything")]).is_err(),
                "host path '{}' was accepted as a project folder, which bind-mounts a whole \
                 filesystem read-write into a container with passwordless sudo",
                escape
            );
            // The same value must not be reachable through the editor either.
            assert!(
                validate_project_paths_update(&[], &[path(escape, "everything")]).is_err(),
                "host path '{}' was accepted through update_project",
                escape
            );
            // And the two read-only mounts are the same check.
            assert!(
                validate_mounted_host_path("the SSH key folder", None, Some(escape)).is_err(),
                "'{}' was accepted as an SSH key path, which read-only bind-mounts a whole \
                 filesystem at /tmp/.host-ssh",
                escape
            );
        }
    }

    /// A drive-relative path (`C:x`, no separator) means "x under whatever the
    /// current directory on C: happens to be" — a location decided by the
    /// process rather than by the user, so it may be the drive root.
    #[test]
    fn a_relative_path_is_refused_however_it_resolves_from_here() {
        // The previous test for this passed by coincidence: its four examples
        // did not exist under `app/src-tauri`, so `canonicalize` failed and the
        // `NotAbsolute` branch fired for the wrong reason. Creating a directory
        // named `project` there flipped it red.
        //
        // These are paths that *do* exist relative to wherever the test runs,
        // so they exercise the branch that used to be unreachable. Judged on
        // the typed string, the answer is the same from any working directory —
        // which is the property that matters, because the daemon refuses a
        // relative mount source and the project would save fine and then never
        // start.
        for existing in [".", "..", "src", "./src"] {
            assert!(
                matches!(
                    classify_mount_source(existing),
                    Some(UnmountableHostPath::NotAbsolute)
                ),
                "{} is relative and must be refused regardless of cwd",
                existing
            );
        }

        // And the fix must not have made an absolute path unreachable.
        assert!(
            classify_mount_source("/usr").is_none(),
            "an ordinary absolute folder must still be accepted"
        );
    }

    #[test]
    fn a_path_that_names_no_location_is_refused_rather_than_guessed_at() {
        for relative in ["C:x", "C:Users\\jo", "relative/path", "./project"] {
            assert!(
                validate_project_paths(&[path(relative, "project")]).is_err(),
                "'{}' was accepted, though where it lands depends on Triple-C's own \
                 working directory",
                relative
            );
        }
    }

    /// The dots that are *not* an escape have to keep working — a folder can
    /// legitimately be reached through `.` or a `..` that goes back down again,
    /// and the Browse button produces paths on machines this list is not
    /// running on.
    #[test]
    fn an_ordinary_folder_is_still_accepted_however_it_is_spelled() {
        for ok in [
            "/home/u/./project",
            "/home/u/x/../project",
            "/home/u/..project",
            "/home/u/project/..hidden",
            "C:\\Users\\u\\x\\..\\project",
            "\\\\server\\share\\project",
            "\\\\?\\C:\\Users\\u\\project",
        ] {
            assert!(
                validate_project_paths(&[path(ok, "project")]).is_ok(),
                "host path '{}' should be usable",
                ok
            );
        }
    }

    /// The half of this that only resolution can answer.
    ///
    /// A lexical check sees a two-component path under `/tmp` and stops. The
    /// container is what plants the link — `/proc/self/mountinfo` inside a
    /// Triple-C container spells the host's project paths out verbatim — so the
    /// symlink is reachable, and the mount that follows it is read-write.
    #[cfg(unix)]
    #[test]
    fn a_symlink_to_the_root_is_refused_because_resolution_is_what_answers() {
        let dir = std::env::temp_dir().join(format!(
            "triple-c-root-link-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let link = dir.join("innocent");
        std::os::unix::fs::symlink("/", &link).unwrap();

        let verdict = validate_project_paths(&[path(&link.to_string_lossy(), "project")]);
        std::fs::remove_file(&link).ok();
        std::fs::remove_dir(&dir).ok();

        assert!(
            verdict.is_err(),
            "a symlink to / was accepted as a project folder; only canonicalisation can see it"
        );
    }

    #[test]
    fn duplicate_and_half_filled_rows_are_refused_but_the_blank_row_is_not() {
        assert!(validate_project_paths(&[
            path("/home/u/a", "same"),
            path("/home/u/b", "same"),
        ])
        .is_err());
        assert!(validate_project_paths(&[path("/home/u/a", "")]).is_err());
        assert!(validate_project_paths(&[path("", "a")]).is_err());
        // "+ Add folder" inserts this and the next blur saves the whole list;
        // refusing it would turn an empty row into an error toast.
        assert!(validate_project_paths(&[path("/home/u/a", "a"), path("", "")]).is_ok());
    }

    /// The distinction the whole secret-clearing path rests on.
    ///
    /// Every secret field is `#[serde(skip_serializing)]`, so the project
    /// object the frontend holds has no `git_token` key — a save from the
    /// Workspace or Runtime section sends it *absent*, and clearing on absent
    /// would delete the token every time an unrelated setting was changed. The
    /// editor that owns the field sends an explicit `null`.
    #[test]
    fn a_blanked_secret_is_cleared_and_an_absent_one_is_left_alone() {
        let blanked = serde_json::json!({
            "id": "p1",
            "git_token": null,
            "bedrock_config": { "aws_bearer_token": null },
        });
        let cleared = explicitly_cleared_secrets(&blanked);
        assert!(cleared.contains(&"git-token"));
        assert!(cleared.contains(&"aws-bearer-token"));
        // Present in the same payload, absent from the clear list.
        assert!(!cleared.contains(&"aws-access-key-id"));

        // What a Workspace-section save looks like: no secret keys at all.
        let unrelated = serde_json::json!({ "id": "p1", "name": "renamed" });
        assert!(explicitly_cleared_secrets(&unrelated).is_empty());

        // And a value is neither.
        let written = serde_json::json!({ "id": "p1", "git_token": "ghp_xxx" });
        assert!(explicitly_cleared_secrets(&written).is_empty());
    }

    #[test]
    fn every_secret_field_can_be_cleared() {
        // A field the frontend can blank but this list does not name is a
        // credential that cannot be revoked through the UI, which is the bug
        // this exists to close. The keychain keys must match the ones
        // `load_secrets_for_project` reads back, or a save writes one name and
        // the container is handed another.
        for (pointer, key) in PROJECT_SECRET_FIELDS {
            let field = pointer.rsplit('/').next().unwrap();
            let payload = match pointer.matches('/').count() {
                1 => serde_json::json!({ field: null }),
                _ => {
                    let parent = pointer.trim_start_matches('/').split('/').next().unwrap();
                    serde_json::json!({ parent: { field: null } })
                }
            };
            assert!(
                explicitly_cleared_secrets(&payload).contains(key),
                "{} does not reach the keychain key {}",
                pointer,
                key
            );
        }
    }

    #[test]
    fn add_and_update_cannot_disagree_about_what_a_folder_list_may_contain() {
        // `update_project` used to validate nothing at all, so every rule in
        // `add_project` was one save-on-blur away from being bypassed. It now
        // validates against what is stored rather than in isolation, but a row
        // it has never seen before is held to exactly the same rules — this
        // fails if either side grows its own copy.
        let bad = [path("/home/u/project", "../tmp/claude-x")];
        assert!(validate_project_paths(&bad).is_err());
        assert!(validate_project_paths_update(&[], &bad).is_err());
        let good = [path("/home/u/project", "project")];
        assert!(validate_project_paths(&good).is_ok());
        assert!(validate_project_paths_update(&[], &good).is_ok());
    }

    // ── Folder lists that are already in `projects.json` ──────────────────
    //
    // `update_project` validated nothing while `WorkspaceSection` saved
    // `{paths}` on every blur, so a stored list can break rules that only
    // `add_project` ever enforced. Holding a save to all of them turned every
    // such project into one that cannot be saved *at all* — not its folders:
    // `update_project` is the single command behind the whole Config tab, so a
    // sandbox toggle, a permission-mode change and `useTerminal.ts`'s tab
    // rename all came back with a message about folders.

    /// The shapes a real `projects.json` can be holding. None of them is an
    /// escape from `/workspace`; all of them used to brick the editor.
    fn legacy_rows() -> Vec<Vec<ProjectPath>> {
        vec![
            // Half-filled: "+ Add folder", a host path typed, no name yet, and
            // an unrelated blur saved the list.
            vec![path("/home/u/a", "a"), path("/home/u/b", "")],
            vec![path("/home/u/a", "a"), path("", "b")],
            // A mount name the character check refuses but the daemon puts
            // exactly where it says: /workspace/my project.
            vec![path("/home/u/a", "my project")],
            vec![path("/home/u/a", "web@2")],
            // Two rows sharing a name.
            vec![path("/home/u/a", "same"), path("/home/u/b", "same")],
            // The whole drive, from before anything refused it.
            vec![path("/", "everything")],
            vec![path("C:\\", "everything")],
        ]
    }

    #[test]
    fn a_project_stored_with_a_bad_row_can_still_be_saved() {
        for rows in legacy_rows() {
            // The full rule set is what made these unsavable…
            assert!(
                validate_project_paths(&rows).is_err(),
                "fixture {:?} is not actually a rule violation",
                rows
            );
            // …and an unrelated Config save re-sends the list it was given.
            assert!(
                validate_project_paths_update(&rows, &rows).is_ok(),
                "saving an unrelated setting on a project stored as {:?} is refused, so every \
                 toggle in the Config tab fails with a message about folders",
                rows
            );
        }
    }

    #[test]
    fn the_same_bad_row_is_refused_when_it_is_new() {
        let stored = [path("/home/u/project", "project")];
        for rows in legacy_rows() {
            assert!(
                validate_project_paths_update(&stored, &rows).is_err(),
                "{:?} was introduced through update_project, which is the escalation the \
                 validation exists to stop",
                rows
            );
        }
    }

    /// The one rule no exemption reaches. `/workspace/../tmp/claude-x`
    /// normalises to `/tmp/claude-x` — a path the pre-commit scrub owns and
    /// empties as root — so a stored one is a live data-loss chain rather than
    /// untidy data, and the Workspace section is one edit away.
    #[test]
    fn a_mount_that_leaves_workspace_is_refused_however_it_got_there() {
        for escape in ["..", "../tmp/claude-x", "../../etc", "/tmp/claude-x", "a/../..", ".", "..\\x"] {
            let rows = [path("/home/u/project", escape)];
            assert!(
                validate_project_paths_update(&rows, &rows).is_err(),
                "mount name '{}' was grandfathered, so the C1 chain stays open for anyone who \
                 already has it stored",
                escape
            );
            assert!(validate_project_paths_update(&[], &rows).is_err());
        }
    }

    #[test]
    fn editing_a_grandfathered_row_holds_it_to_every_rule_again() {
        let stored = [path("/", "everything")];
        // Renaming the mount but keeping the root host path is a new row.
        assert!(
            validate_project_paths_update(&stored, &[path("/", "all")]).is_err(),
            "an edited row was admitted on the strength of the row it replaced"
        );
        // Fixing the host path is what the message asks for, and it saves.
        assert!(
            validate_project_paths_update(&stored, &[path("/home/u/a", "everything")]).is_ok()
        );
        // Dropping the row entirely is always fine.
        assert!(validate_project_paths_update(&stored, &[]).is_ok());
    }

    #[test]
    fn a_stored_duplicate_may_be_kept_but_not_multiplied() {
        let stored = [path("/home/u/a", "same"), path("/home/u/b", "same")];
        assert!(validate_project_paths_update(&stored, &stored).is_ok());
        // Order must not change the answer.
        let reordered = [stored[1].clone(), stored[0].clone()];
        assert!(validate_project_paths_update(&stored, &reordered).is_ok());
        // A third row taking the same name is new, and refused.
        let more = [
            stored[0].clone(),
            stored[1].clone(),
            path("/home/u/c", "same"),
        ];
        assert!(validate_project_paths_update(&stored, &more).is_err());
        // And a second copy of a name that was unique stays refused.
        let unique = [path("/home/u/a", "a")];
        assert!(validate_project_paths_update(
            &unique,
            &[path("/home/u/a", "a"), path("/home/u/b", "a")]
        )
        .is_err());
    }

    #[test]
    fn the_blank_placeholder_row_is_still_not_an_error_on_either_path() {
        let stored = [path("/home/u/a", "a")];
        let with_placeholder = [path("/home/u/a", "a"), path("", "")];
        assert!(validate_project_paths(&with_placeholder).is_ok());
        assert!(validate_project_paths_update(&stored, &with_placeholder).is_ok());
    }

    // ── The two host paths that had no check at all ───────────────────────

    #[test]
    fn a_filesystem_root_cannot_be_newly_set_as_an_ssh_or_ca_path() {
        for root in ["/", "//", "\\", "C:\\", "c:/", "D:"] {
            assert!(
                validate_mounted_host_path("the SSH key folder", None, Some(root)).is_err(),
                "'{}' was accepted as an SSH key path, which read-only bind-mounts the whole \
                 host filesystem at /tmp/.host-ssh",
                root
            );
            assert!(
                validate_mounted_host_path("the CA certificate path", Some("/etc/ssl"), Some(root))
                    .is_err(),
                "'{}' was accepted as a CA certificate path",
                root
            );
        }
        // A real folder, a cleared value and an absent one are all fine.
        assert!(validate_mounted_host_path("x", None, Some("/home/u/.ssh")).is_ok());
        assert!(validate_mounted_host_path("x", Some("/home/u/.ssh"), None).is_ok());
        assert!(validate_mounted_host_path("x", Some("/home/u/.ssh"), Some("")).is_ok());
    }

    #[test]
    fn an_ssh_path_already_stored_does_not_brick_the_editor_either() {
        // Nothing ever validated this field, so it can hold a root today — and
        // it is mounted on every container start whether or not an unrelated
        // Config save is allowed through.
        assert!(validate_mounted_host_path("x", Some("/"), Some("/")).is_ok());
        assert!(validate_mounted_host_path("x", Some("/"), Some(" / ")).is_ok());
        // Changing it to a different root is a change, and refused.
        assert!(validate_mounted_host_path("x", Some("/"), Some("C:\\")).is_err());
    }
}
