use serde::{Deserialize, Serialize};

/// One note. A scratchpad entry the user can also fire at a running Claude
/// session.
///
/// Deliberately has no `kind`/`type` field. What makes a note "for the agent"
/// is that the user pressed Send, not a mode chosen when it was written — a
/// classification decision at writing time is one the user is least willing to
/// make, and it would turn one pane into two features.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub body: String,
    /// Pinned notes sort first, then by `updated_at` descending.
    #[serde(default)]
    pub pinned: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl Note {
    pub fn new(title: String, body: String) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title,
            body,
            pinned: false,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}
