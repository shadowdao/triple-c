//! IPC shapes of persistent AWS terminals (`host::persistent`); mirrored in
//! `lib/types.ts`.

use serde::Serialize;

pub use crate::host::persistent::TerminalKind;

/// What `open_terminal_session` tells the tab it opened.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct OpenedTerminal {
    /// Runs in tmux in the workspace and outlives the app (AWS only).
    pub persistent: bool,
    /// Something to say once (the image has no tmux).
    pub notice: Option<String>,
}

/// A terminal session running in an AWS workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceTerminal {
    pub session_id: String,
    pub kind: TerminalKind,
    /// The app's label for it when it opened it, else the label tmux keeps.
    pub name: Option<String>,
    pub created_unix: u64,
    /// A client (this app, another machine, the web terminal) is attached.
    pub attached: bool,
    /// This app opened it and reopens it on connect.
    pub known: bool,
}

/// What `reattach_terminal_session` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum ReattachOutcome {
    /// Attached again; the tab keeps running.
    Reattached { kind: TerminalKind, name: Option<String> },
    /// The session is no longer running in the workspace (it ended, or the
    /// workspace was replaced); it has been forgotten.
    Ended,
}
