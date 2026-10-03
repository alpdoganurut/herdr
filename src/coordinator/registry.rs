//! The agents-v1 registry (`managed.json`), read only.
//!
//! Agents v2 retired it: every tab is part of herdr+, roles and notes live
//! on the pane (`PaneAgentMeta`), and the coordinator is the server's own
//! record. The file is read by the server's one-time migration
//! (`app::agents_migrate`) and for the POC coordinator's session at the first
//! native start, and kept on disk untouched as the rollback source: nothing
//! in v2 writes, renames or deletes it.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{registry_path, COORDINATOR_ROLE};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedAgent {
    /// Native session id (Claude session uuid, Codex thread id), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Last known public pane id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// Agent kind (claude, codex, ...), informational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub added_unix: u64,
}

impl ManagedAgent {
    pub fn is_coordinator(&self) -> bool {
        self.role.as_deref() == Some(COORDINATOR_ROLE)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub agents: Vec<ManagedAgent>,
}

impl Registry {
    /// The entry with the coordinator role (the POC's coordinator).
    pub fn coordinator(&self) -> Option<&ManagedAgent> {
        self.agents.iter().find(|entry| entry.is_coordinator())
    }
}

#[derive(Debug)]
pub enum LoadError {
    Corrupt(String),
    Io(io::Error),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Corrupt(err) => write!(f, "managed.json is corrupt: {err}"),
            Self::Io(err) => write!(f, "cannot read managed.json: {err}"),
        }
    }
}

/// Load the registry, telling a missing file (empty registry) apart from an
/// unreadable or corrupt one.
pub fn load_strict(dir: &Path) -> Result<Registry, LoadError> {
    match std::fs::read(registry_path(dir)) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|err| LoadError::Corrupt(err.to_string()))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Registry::default()),
        Err(err) => Err(LoadError::Io(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_v1_file_and_tells_missing_from_corrupt() {
        let dir = super::super::test_dir("registry-read");
        assert_eq!(load_strict(&dir).unwrap(), Registry::default(), "missing");
        std::fs::write(
            registry_path(&dir),
            r#"{"agents":[{"session":"s","pane_id":"w1:p1","agent":"claude","role":"coordinator","added_unix":1},{"pane_id":"w2:p2","role":"lead","project":"app","extra":true}]}"#,
        )
        .unwrap();
        let registry = load_strict(&dir).unwrap();
        assert_eq!(registry.agents.len(), 2);
        assert_eq!(
            registry.coordinator().and_then(|c| c.session.as_deref()),
            Some("s")
        );
        assert_eq!(registry.agents[1].project.as_deref(), Some("app"));
        std::fs::write(registry_path(&dir), "{oops").unwrap();
        let err = load_strict(&dir).unwrap_err();
        assert!(
            err.to_string().starts_with("managed.json is corrupt"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
