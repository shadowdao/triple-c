//! Which persistent AWS terminals (`host::persistent`) the app has open, per
//! project: `<data_dir>/triple-c/terminal-sessions.json`.
//!
//! The session id is the tmux key, so this is what lets the app reopen a
//! project's tabs — same ids, kinds and labels — after it was closed. It is
//! a hint, never the truth: the workspace's own listing decides what is
//! running, and an entry it does not list is forgotten (`retain`).
//!
//! Not a `Project` field, for the reason notes are not: the webview saves
//! whole projects, and a tab opening while a Config save is in flight would
//! lose one of the two. Same free-function shape as `notes_store`, with
//! `_in` variants the tests drive against a temp directory. An unreadable
//! file is treated as empty: losing it costs only the automatic reopen (the
//! sessions still show in Project Home → Sessions).

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::host::persistent::{validate_key, TerminalKind};

const FORMAT_VERSION: u32 = 1;
/// More tabs than anyone keeps open: a bound on what a bug could grow.
const MAX_PER_PROJECT: usize = 64;

/// One open persistent terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedTerminal {
    /// The tab's session id, which is also the tmux key.
    pub id: String,
    pub kind: TerminalKind,
    /// The label the tab was opened with (a rename lives in
    /// `Project::renamed_session_names`, keyed by the same id).
    pub name: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    version: u32,
    #[serde(default)]
    projects: BTreeMap<String, Vec<PersistedTerminal>>,
    /// Tabs closed while the workspace could not be reached: their tmux
    /// servers still run (a leftover Claude would keep a billed workspace
    /// up), so they are ended at the next connect.
    #[serde(default)]
    pending_end: BTreeMap<String, Vec<String>>,
}

fn lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

fn data_dir() -> Result<PathBuf, String> {
    let dir = dirs::data_dir()
        .ok_or_else(|| "Could not determine data directory".to_string())?
        .join("triple-c");
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create data directory: {e}"))?;
    Ok(dir)
}

fn path_in(dir: &Path) -> PathBuf {
    dir.join("terminal-sessions.json")
}

fn load_file(dir: &Path) -> File {
    let path = path_in(dir);
    let Ok(data) = fs::read_to_string(&path) else {
        return File::default();
    };
    match serde_json::from_str::<File>(&data) {
        Ok(f) => f,
        Err(e) => {
            log::warn!("{} is unreadable ({e}); starting with no remembered terminals", path.display());
            File::default()
        }
    }
}

fn save_file(dir: &Path, mut file: File) -> Result<(), String> {
    file.version = FORMAT_VERSION;
    file.projects.retain(|_, v| !v.is_empty());
    file.pending_end.retain(|_, v| !v.is_empty());
    let path = path_in(dir);
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    fs::write(&tmp, data).map_err(|e| format!("Failed to write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|e| format!("Failed to commit {}: {e}", path.display()))
}

fn edit_in<T>(dir: &Path, f: impl FnOnce(&mut File) -> T) -> Result<T, String> {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut file = load_file(dir);
    let out = f(&mut file);
    save_file(dir, file)?;
    Ok(out)
}

// ── `_in` variants (tests) ────────────────────────────────────────────────

pub fn list_in(dir: &Path, project_id: &str) -> Vec<PersistedTerminal> {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    load_file(dir).projects.remove(project_id).unwrap_or_default()
}

pub fn add_in(dir: &Path, project_id: &str, t: PersistedTerminal) -> Result<(), String> {
    validate_key(&t.id)?;
    edit_in(dir, |f| {
        let list = f.projects.entry(project_id.to_string()).or_default();
        if let Some(existing) = list.iter_mut().find(|e| e.id == t.id) {
            *existing = t;
        } else if list.len() < MAX_PER_PROJECT {
            list.push(t);
        } else {
            log::warn!("Project {project_id}: too many remembered terminals; not remembering another");
        }
    })
}

/// Forget `session_id`; returns the project it belonged to.
pub fn remove_in(dir: &Path, session_id: &str) -> Result<Option<String>, String> {
    edit_in(dir, |f| {
        let mut owner = None;
        for (pid, list) in f.projects.iter_mut() {
            let before = list.len();
            list.retain(|e| e.id != session_id);
            if list.len() != before {
                owner = Some(pid.clone());
            }
        }
        owner
    })
}

/// The project and record of `session_id`, if remembered.
pub fn find_in(dir: &Path, session_id: &str) -> Option<(String, PersistedTerminal)> {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    load_file(dir).projects.into_iter().find_map(|(pid, list)| {
        list.into_iter().find(|e| e.id == session_id).map(|e| (pid, e))
    })
}

/// Keep only the project's entries the workspace still runs.
pub fn retain_in(dir: &Path, project_id: &str, running: &HashSet<String>) -> Result<(), String> {
    edit_in(dir, |f| {
        if let Some(list) = f.projects.get_mut(project_id) {
            list.retain(|e| running.contains(&e.id));
        }
    })
}

/// Forget all of a project's terminals (stopped, removed, moved off AWS):
/// remembered and pending-end alike.
pub fn clear_in(dir: &Path, project_id: &str) -> Result<(), String> {
    edit_in(dir, |f| {
        f.projects.remove(project_id);
        f.pending_end.remove(project_id);
    })
}

/// Remember to end `session_id` at the project's next connect.
pub fn add_pending_end_in(dir: &Path, project_id: &str, session_id: &str) -> Result<(), String> {
    validate_key(session_id)?;
    edit_in(dir, |f| {
        let list = f.pending_end.entry(project_id.to_string()).or_default();
        if !list.iter().any(|k| k == session_id) && list.len() < MAX_PER_PROJECT {
            list.push(session_id.to_string());
        }
    })
}

/// The project's sessions waiting to be ended.
pub fn pending_end_in(dir: &Path, project_id: &str) -> Vec<String> {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    load_file(dir).pending_end.remove(project_id).unwrap_or_default()
}

/// They were ended (or are gone): stop remembering them.
pub fn done_pending_end_in(dir: &Path, project_id: &str, ended: &[String]) -> Result<(), String> {
    edit_in(dir, |f| {
        if let Some(list) = f.pending_end.get_mut(project_id) {
            list.retain(|k| !ended.contains(k));
        }
    })
}

// ── Public API ────────────────────────────────────────────────────────────

pub fn list(project_id: &str) -> Vec<PersistedTerminal> {
    data_dir().map(|d| list_in(&d, project_id)).unwrap_or_default()
}

pub fn add(project_id: &str, t: PersistedTerminal) -> Result<(), String> {
    add_in(&data_dir()?, project_id, t)
}

pub fn remove(session_id: &str) -> Result<Option<String>, String> {
    remove_in(&data_dir()?, session_id)
}

pub fn find(session_id: &str) -> Option<(String, PersistedTerminal)> {
    data_dir().ok().and_then(|d| find_in(&d, session_id))
}

pub fn retain(project_id: &str, running: &HashSet<String>) -> Result<(), String> {
    retain_in(&data_dir()?, project_id, running)
}

pub fn add_pending_end(project_id: &str, session_id: &str) -> Result<(), String> {
    add_pending_end_in(&data_dir()?, project_id, session_id)
}

pub fn pending_end(project_id: &str) -> Vec<String> {
    data_dir().map(|d| pending_end_in(&d, project_id)).unwrap_or_default()
}

pub fn done_pending_end(project_id: &str, ended: &[String]) -> Result<(), String> {
    done_pending_end_in(&data_dir()?, project_id, ended)
}

/// Best effort: a failure is logged (the next listing prunes anyway).
pub fn clear(project_id: &str) {
    match data_dir().and_then(|d| clear_in(&d, project_id)) {
        Ok(()) => {}
        Err(e) => log::warn!("Could not forget project {project_id}'s terminals: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tc-term-store-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn t(id: &str, kind: TerminalKind, name: Option<&str>) -> PersistedTerminal {
        PersistedTerminal { id: id.into(), kind, name: name.map(str::to_string) }
    }

    #[test]
    fn terminals_are_remembered_per_project_in_order_and_survive_a_reload() {
        let d = dir("order");
        add_in(&d, "p1", t("a-1", TerminalKind::Claude, Some("main"))).unwrap();
        add_in(&d, "p1", t("b-2", TerminalKind::Bash, None)).unwrap();
        add_in(&d, "p2", t("c-3", TerminalKind::Bash, None)).unwrap();
        // Re-adding replaces in place (a reattach records it again).
        add_in(&d, "p1", t("a-1", TerminalKind::Claude, Some("renamed"))).unwrap();
        let p1 = list_in(&d, "p1");
        assert_eq!(p1.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["a-1", "b-2"]);
        assert_eq!(p1[0].name.as_deref(), Some("renamed"));
        assert_eq!(find_in(&d, "c-3").unwrap().0, "p2");
        let raw = fs::read_to_string(path_in(&d)).unwrap();
        assert!(raw.contains("\"version\": 1") && raw.contains("\"kind\": \"claude\""), "{raw}");
    }

    #[test]
    fn closing_pruning_and_stopping_forget_entries() {
        let d = dir("forget");
        for id in ["a", "b", "c"] {
            add_in(&d, "p", t(id, TerminalKind::Bash, None)).unwrap();
        }
        assert_eq!(remove_in(&d, "b").unwrap().as_deref(), Some("p"));
        assert_eq!(remove_in(&d, "zzz").unwrap(), None);
        retain_in(&d, "p", &HashSet::from(["c".to_string()])).unwrap();
        assert_eq!(list_in(&d, "p"), vec![t("c", TerminalKind::Bash, None)]);
        clear_in(&d, "p").unwrap();
        assert!(list_in(&d, "p").is_empty());
    }

    #[test]
    fn a_tab_closed_while_disconnected_is_ended_at_the_next_connect() {
        let d = dir("pending");
        add_pending_end_in(&d, "p", "a-1").unwrap();
        add_pending_end_in(&d, "p", "a-1").unwrap();
        add_pending_end_in(&d, "p", "b-2").unwrap();
        add_pending_end_in(&d, "q", "c-3").unwrap();
        assert!(add_pending_end_in(&d, "p", "../x").is_err());
        assert_eq!(pending_end_in(&d, "p"), ["a-1", "b-2"]);
        done_pending_end_in(&d, "p", &["a-1".to_string()]).unwrap();
        assert_eq!(pending_end_in(&d, "p"), ["b-2"]);
        clear_in(&d, "p").unwrap();
        assert!(pending_end_in(&d, "p").is_empty());
        assert_eq!(pending_end_in(&d, "q"), ["c-3"], "per project");
    }

    #[test]
    fn a_bad_key_is_refused_and_a_corrupt_file_reads_as_empty() {
        let d = dir("bad");
        assert!(add_in(&d, "p", t("../x", TerminalKind::Bash, None)).is_err());
        fs::write(path_in(&d), "{not json").unwrap();
        assert!(list_in(&d, "p").is_empty());
        // …and is replaced by the next write.
        add_in(&d, "p", t("ok", TerminalKind::Bash, None)).unwrap();
        assert_eq!(list_in(&d, "p").len(), 1);
    }
}
