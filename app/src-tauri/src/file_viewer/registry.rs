//! Which viewer window is looking at what.
//!
//! Managed with `app.manage(ViewerRegistry::default())` rather than as a field on
//! `AppState`, like the browser view keeps its own state. A label is reserved *before*
//! the window is built so two concurrent clicks cannot both pass the cap check.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::{MAX_VIEWER_WINDOWS, VIEWER_LABEL_PREFIX};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Location {
    pub line: Option<u32>,
    pub col: Option<u32>,
    pub end_line: Option<u32>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewerTargetState {
    Resolved { container_path: String },
    Choose { candidates: Vec<String> },
    NotFound { tried: Vec<String> },
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ViewerTarget {
    pub project_id: String,
    pub project_name: String,
    pub raw_path: String,
    pub state: ViewerTargetState,
    pub initial: Location,
}

/// What [`ViewerRegistry::reserve`] decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reservation {
    /// A window is already registered on this file. `built` is false while that
    /// window is still being created: it has no `WebviewWindow` to focus yet, and
    /// it will open at its own location, so the caller should simply return.
    Existing { label: String, built: bool },
    /// A new label, registered and counted against the cap; build its window,
    /// then call [`ViewerRegistry::mark_built`] (or `remove` if building failed).
    Reserved(String),
}

/// What [`ViewerRegistry::choose`] decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// The caller's entry now points at the chosen file.
    Resolved(ViewerTarget),
    /// Another window already has that file; the caller's entry is unchanged.
    AlreadyOpen { label: String, built: bool },
}

#[derive(Clone, Debug)]
struct Entry {
    target: ViewerTarget,
    /// Set once the window's `build()` has returned. Until then the label has no
    /// window by design, so "registered but windowless" means "being built", not
    /// "stale" — only built entries are ever pruned.
    built: bool,
}

#[derive(Default)]
pub struct ViewerRegistry {
    entries: Mutex<HashMap<String, Entry>>,
    next: AtomicU64,
}

fn same_file(t: &ViewerTarget, project_id: &str, container_path: &str) -> bool {
    t.project_id == project_id
        && matches!(&t.state, ViewerTargetState::Resolved { container_path: p } if p == container_path)
}

fn open_on(
    entries: &HashMap<String, Entry>,
    project_id: &str,
    container_path: &str,
    except: Option<&str>,
) -> Option<(String, bool)> {
    entries
        .iter()
        .find(|(label, e)| Some(label.as_str()) != except && same_file(&e.target, project_id, container_path))
        .map(|(label, e)| (label.clone(), e.built))
}

/// Drops built entries whose window is gone, whatever their state. `Destroyed`
/// normally removes an entry; this is the backstop for one it missed, so a leak
/// can never hold a cap slot for good.
fn prune(entries: &mut HashMap<String, Entry>, is_live: &dyn Fn(&str) -> bool) {
    entries.retain(|label, e| !e.built || is_live(label));
}

impl ViewerRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Finds the window already open on a resolved target, or reserves a label,
    /// in one critical section, after pruning built entries `is_live` says are
    /// gone. `is_live` runs under the registry lock and must not call back into
    /// the registry.
    pub fn reserve(
        &self,
        target: ViewerTarget,
        is_live: impl Fn(&str) -> bool,
    ) -> Result<Reservation, String> {
        let mut entries = self.lock();
        prune(&mut entries, &is_live);
        if let ViewerTargetState::Resolved { container_path } = &target.state {
            if let Some((label, built)) = open_on(&entries, &target.project_id, container_path, None) {
                return Ok(Reservation::Existing { label, built });
            }
        }
        if entries.len() >= MAX_VIEWER_WINDOWS {
            return Err(format!(
                "{} file windows are already open — close one before opening another.",
                MAX_VIEWER_WINDOWS
            ));
        }
        let n = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let label = format!("{}{}", VIEWER_LABEL_PREFIX, n);
        entries.insert(label.clone(), Entry { target, built: false });
        Ok(Reservation::Reserved(label))
    }

    /// Records that `label`'s window exists. A no-op if it was already removed
    /// (a window destroyed the moment it appeared).
    pub fn mark_built(&self, label: &str) {
        if let Some(e) = self.lock().get_mut(label) {
            e.built = true;
        }
    }

    /// Points `label`'s entry at `container_path`, unless another window already
    /// has that file open — then the entry is left alone, so no two entries are
    /// ever resolved to the same file.
    pub fn choose(
        &self,
        label: &str,
        container_path: String,
        is_live: impl Fn(&str) -> bool,
    ) -> Result<Choice, String> {
        let mut entries = self.lock();
        prune(&mut entries, &is_live);
        let project_id = entries
            .get(label)
            .ok_or_else(|| "This file window is no longer registered.".to_string())?
            .target
            .project_id
            .clone();
        if let Some((other, built)) = open_on(&entries, &project_id, &container_path, Some(label)) {
            return Ok(Choice::AlreadyOpen { label: other, built });
        }
        let entry = entries.get_mut(label).expect("checked above under the same lock");
        entry.target.state = ViewerTargetState::Resolved { container_path };
        Ok(Choice::Resolved(entry.target.clone()))
    }

    pub fn get(&self, label: &str) -> Option<ViewerTarget> {
        self.lock().get(label).map(|e| e.target.clone())
    }

    pub fn set_state(&self, label: &str, state: ViewerTargetState) -> Result<ViewerTarget, String> {
        let mut entries = self.lock();
        let entry = entries
            .get_mut(label)
            .ok_or_else(|| "This file window is no longer registered.".to_string())?;
        entry.target.state = state;
        Ok(entry.target.clone())
    }

    pub fn remove(&self, label: &str) {
        self.lock().remove(label);
    }

    pub fn find_open(&self, project_id: &str, container_path: &str) -> Option<String> {
        open_on(&self.lock(), project_id, container_path, None).map(|(label, _)| label)
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(project: &str, path: &str) -> ViewerTarget {
        ViewerTarget {
            project_id: project.into(),
            project_name: "Demo".into(),
            raw_path: path.into(),
            state: ViewerTargetState::Resolved { container_path: path.into() },
            initial: Location { line: Some(3), col: None, end_line: None },
        }
    }

    fn all_live(_: &str) -> bool {
        true
    }

    /// Reserves a label that must be new.
    fn fresh(r: &ViewerRegistry, t: ViewerTarget) -> String {
        match r.reserve(t, all_live).unwrap() {
            Reservation::Reserved(label) => label,
            other => panic!("expected a new label, got {:?}", other),
        }
    }

    fn choosing(project: &str, candidates: &[&str]) -> ViewerTarget {
        ViewerTarget {
            state: ViewerTargetState::Choose { candidates: candidates.iter().map(|c| c.to_string()).collect() },
            ..target(project, "a")
        }
    }

    #[test]
    fn labels_are_sequential_and_never_reused() {
        let r = ViewerRegistry::default();
        let a = fresh(&r, target("p", "/workspace/a"));
        let b = fresh(&r, target("p", "/workspace/b"));
        assert_eq!(a, "file-viewer-1");
        assert_eq!(b, "file-viewer-2");
        r.remove(&a);
        let c = fresh(&r, target("p", "/workspace/c"));
        assert_eq!(c, "file-viewer-3");
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn the_cap_refuses_the_twenty_first_window() {
        let r = ViewerRegistry::default();
        for i in 0..MAX_VIEWER_WINDOWS {
            fresh(&r, target("p", &format!("/workspace/{}", i)));
        }
        let err = r.reserve(target("p", "/workspace/one-more"), all_live).unwrap_err();
        assert!(err.contains("20"), "{}", err);
        assert_eq!(r.len(), MAX_VIEWER_WINDOWS);
    }

    #[test]
    fn an_open_resolved_file_is_found_by_project_and_path() {
        let r = ViewerRegistry::default();
        let label = fresh(&r, target("p", "/workspace/a"));
        assert_eq!(r.find_open("p", "/workspace/a"), Some(label.clone()));
        assert_eq!(r.find_open("other", "/workspace/a"), None);
        // A window still choosing is not "open on" any path.
        r.set_state(&label, ViewerTargetState::Choose { candidates: vec!["/workspace/a".into()] }).unwrap();
        assert_eq!(r.find_open("p", "/workspace/a"), None);
        r.remove(&label);
        assert_eq!(r.get(&label), None);
    }

    #[test]
    fn set_state_on_an_unknown_label_is_an_error() {
        let r = ViewerRegistry::default();
        assert!(r.set_state("file-viewer-9", ViewerTargetState::NotFound { tried: vec![] }).is_err());
    }

    #[test]
    fn target_state_serialises_with_a_kind_tag() {
        let s = serde_json::to_string(&ViewerTargetState::NotFound { tried: vec!["/x".into()] }).unwrap();
        assert_eq!(s, r#"{"kind":"not_found","tried":["/x"]}"#);
    }

    /// I1: a second click while the first window is still being built must find
    /// that window, not read it as stale and reserve a second one.
    #[test]
    fn a_window_being_built_is_found_not_replaced() {
        let r = ViewerRegistry::default();
        let a = fresh(&r, target("p", "/workspace/a"));
        // No window exists yet for `a`: `is_live` says so, and it must not matter.
        let second = r.reserve(target("p", "/workspace/a"), |_| false).unwrap();
        assert_eq!(second, Reservation::Existing { label: a.clone(), built: false });
        assert!(r.get(&a).is_some());
        assert_eq!(r.len(), 1);

        r.mark_built(&a);
        let third = r.reserve(target("p", "/workspace/a"), all_live).unwrap();
        assert_eq!(third, Reservation::Existing { label: a, built: true });
        assert_eq!(r.len(), 1);
    }

    /// A built entry whose window is gone is stale: pruned, and the file reopens.
    #[test]
    fn a_built_entry_without_a_window_is_pruned_and_the_file_reopens() {
        let r = ViewerRegistry::default();
        let a = fresh(&r, target("p", "/workspace/a"));
        r.mark_built(&a);
        let again = r.reserve(target("p", "/workspace/a"), |_| false).unwrap();
        assert_eq!(again, Reservation::Reserved("file-viewer-2".into()));
        assert_eq!(r.get(&a), None);
        assert_eq!(r.len(), 1);
    }

    /// M2: a leaked entry of any state cannot hold a cap slot once built and gone,
    /// and an entry still being built always keeps its slot.
    #[test]
    fn leaked_entries_of_every_state_free_their_cap_slot() {
        let r = ViewerRegistry::default();
        let mut labels = Vec::new();
        for i in 0..MAX_VIEWER_WINDOWS {
            let t = match i % 3 {
                0 => target("p", &format!("/workspace/{}", i)),
                1 => choosing("p", &["/workspace/x", "/workspace/y"]),
                _ => ViewerTarget { state: ViewerTargetState::NotFound { tried: vec![] }, ..target("p", "z") },
            };
            labels.push(fresh(&r, t));
        }
        // All still being built: none may be pruned, so the cap holds.
        assert!(r.reserve(target("p", "/workspace/new"), |_| false).is_err());
        for l in &labels {
            r.mark_built(l);
        }
        // Built, and one of each state has lost its window.
        let dead = [labels[0].clone(), labels[1].clone(), labels[2].clone()];
        let live = |l: &str| !dead.iter().any(|d| d == l);
        assert!(matches!(r.reserve(target("p", "/workspace/new"), live), Ok(Reservation::Reserved(_))));
        assert_eq!(r.len(), MAX_VIEWER_WINDOWS - 2);
        for d in &dead {
            assert_eq!(r.get(d), None);
        }
    }

    #[test]
    fn mark_built_on_a_removed_label_is_a_no_op() {
        let r = ViewerRegistry::default();
        let a = fresh(&r, target("p", "/workspace/a"));
        r.remove(&a);
        r.mark_built(&a);
        assert_eq!(r.get(&a), None);
    }

    /// M5: choosing a file another window already has leaves the chooser alone,
    /// so two entries are never resolved to the same file.
    #[test]
    fn choosing_a_file_open_elsewhere_does_not_resolve_a_second_entry() {
        let r = ViewerRegistry::default();
        let open = fresh(&r, target("p", "/workspace/x"));
        r.mark_built(&open);
        let chooser = fresh(&r, choosing("p", &["/workspace/x", "/workspace/y"]));
        r.mark_built(&chooser);

        let c = r.choose(&chooser, "/workspace/x".into(), all_live).unwrap();
        assert_eq!(c, Choice::AlreadyOpen { label: open.clone(), built: true });
        assert!(matches!(r.get(&chooser).unwrap().state, ViewerTargetState::Choose { .. }));

        match r.choose(&chooser, "/workspace/y".into(), all_live).unwrap() {
            Choice::Resolved(t) => assert_eq!(t.state, ViewerTargetState::Resolved { container_path: "/workspace/y".into() }),
            other => panic!("expected Resolved, got {:?}", other),
        }
        assert_eq!(r.find_open("p", "/workspace/y"), Some(chooser));
    }

    #[test]
    fn choosing_the_same_path_in_another_project_is_not_a_duplicate() {
        let r = ViewerRegistry::default();
        fresh(&r, target("other", "/workspace/x"));
        let chooser = fresh(&r, choosing("p", &["/workspace/x"]));
        assert!(matches!(r.choose(&chooser, "/workspace/x".into(), all_live), Ok(Choice::Resolved(_))));
    }

    #[test]
    fn choose_on_an_unknown_label_is_an_error() {
        let r = ViewerRegistry::default();
        assert!(r.choose("file-viewer-9", "/workspace/x".into(), all_live).is_err());
    }
}
