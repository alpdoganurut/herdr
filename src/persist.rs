//! Session persistence — save/restore workspaces, layouts, and working directories.
//!
//! Stored at `~/.config/herdr/session.json`.
//! Optional pane screen history is stored separately at `session-history.json`.
//! Installed plugins are persisted separately at `plugins.json`.
//! Native agent transcript backups live under `agent-transcripts/`.
//! Recently closed agent sessions are recorded in `closed-sessions.json`.
//! The AI news desk's schedule and run in flight live in `news.json`.

pub mod agent_transcripts;
pub mod closed_sessions;
mod io;
pub mod news;
pub mod plugin_registry;
mod restore;
mod snapshot;
mod writer;

pub use self::io::{clear_history, load, load_history};
pub use self::restore::restore;
#[cfg(unix)]
pub use self::restore::{handoff_pane_aliases, restore_handoff};
pub use self::snapshot::{
    capture, capture_history, DirectionSnapshot, LayoutSnapshot, SessionHistorySnapshot,
    SessionSnapshot, TabSnapshot, WorkspaceSnapshot,
};
pub(crate) use self::writer::SessionWriter;
