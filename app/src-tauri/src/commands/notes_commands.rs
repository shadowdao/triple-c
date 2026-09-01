use crate::models::Note;
use crate::storage::notes_store;

/// Every project's notes, oldest concept first: pinned notes, then most
/// recently edited.
///
/// Sorted here rather than in the webview so the dock and the tab — two views
/// of the same list — cannot drift into two different orders.
#[tauri::command]
pub async fn list_notes(project_id: String) -> Result<Vec<Note>, String> {
    let mut notes = notes_store::load(&project_id)?;
    notes.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });
    Ok(notes)
}

/// Insert or replace one note.
///
/// There is deliberately no whole-list setter. A bulk write is exactly the
/// clobbering this store's per-project file exists to avoid, and every caller
/// here is editing one note.
#[tauri::command]
pub async fn save_note(project_id: String, note: Note) -> Result<Note, String> {
    notes_store::upsert(&project_id, note)
}

#[tauri::command]
pub async fn delete_note(project_id: String, note_id: String) -> Result<(), String> {
    notes_store::delete(&project_id, &note_id)
}
