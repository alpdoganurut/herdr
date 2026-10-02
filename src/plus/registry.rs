//! The managed-agent registry (`managed.json`): which agents are part of
//! herdr+ and their role, project and note. It is the source of truth for
//! opt-in; pane ids are remapped when a session is restored, so an entry is
//! matched by the agent's native session id first and by pane id second, and
//! a hit refreshes whichever key went stale.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{now_unix, registry_path, write_atomically, COORDINATOR_ROLE};

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

/// What a caller wants to set on a managed agent; `None` keeps the value,
/// `Some("")` clears it.
#[derive(Debug, Clone, Default)]
pub struct ManagePatch {
    pub role: Option<String>,
    pub project: Option<String>,
    pub note: Option<String>,
}

fn apply(field: &mut Option<String>, value: &Option<String>) {
    if let Some(value) = value {
        let value = value.trim();
        *field = (!value.is_empty()).then(|| value.to_string());
    }
}

impl Registry {
    /// Lossy load for read-only callers: a missing or corrupt file reads as an
    /// empty registry. Never `save` what this returns; mutate through [`update`].
    pub fn load(dir: &Path) -> Registry {
        std::fs::read(registry_path(dir))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        write_atomically(&registry_path(dir), &json)
    }

    /// The entry for a live agent: session id first, then pane id (an entry
    /// with a different known session id never matches by pane: the pane now
    /// hosts another conversation).
    pub fn find(&self, session: Option<&str>, pane_id: Option<&str>) -> Option<usize> {
        if let Some(session) = session.filter(|s| !s.is_empty()) {
            if let Some(index) = self
                .agents
                .iter()
                .position(|entry| entry.session.as_deref() == Some(session))
            {
                return Some(index);
            }
        }
        let pane_id = pane_id.filter(|p| !p.is_empty())?;
        self.agents.iter().position(|entry| {
            entry.pane_id.as_deref() == Some(pane_id)
                && match (entry.session.as_deref(), session) {
                    (Some(known), Some(live)) => known == live,
                    _ => true,
                }
        })
    }

    /// Refresh an entry's keys from the live agent it matched; `true` when changed.
    pub fn relink(
        &mut self,
        index: usize,
        session: Option<&str>,
        pane_id: Option<&str>,
        agent: Option<&str>,
    ) -> bool {
        let Some(entry) = self.agents.get_mut(index) else {
            return false;
        };
        let before = entry.clone();
        if let Some(session) = session.filter(|s| !s.is_empty()) {
            entry.session = Some(session.to_string());
        }
        if let Some(pane_id) = pane_id.filter(|p| !p.is_empty()) {
            entry.pane_id = Some(pane_id.to_string());
        }
        if let Some(agent) = agent.filter(|a| !a.is_empty()) {
            entry.agent = Some(agent.to_string());
        }
        *entry != before
    }

    /// Opt an agent in (or update it). Taking the coordinator role is refused
    /// while another entry holds it.
    pub fn manage(
        &mut self,
        session: Option<&str>,
        pane_id: Option<&str>,
        agent: Option<&str>,
        patch: &ManagePatch,
    ) -> Result<ManagedAgent, String> {
        let existing = self.find(session, pane_id);
        if patch.role.as_deref().map(str::trim) == Some(COORDINATOR_ROLE) {
            if let Some(holder) = self
                .agents
                .iter()
                .enumerate()
                .find(|(index, entry)| entry.is_coordinator() && Some(*index) != existing)
            {
                return Err(format!(
                    "pane {} is already the coordinator agent; there is only one",
                    holder.1.pane_id.as_deref().unwrap_or("?")
                ));
            }
        }
        let index = match existing {
            Some(index) => index,
            None => {
                self.agents.push(ManagedAgent {
                    added_unix: now_unix(),
                    ..ManagedAgent::default()
                });
                self.agents.len() - 1
            }
        };
        self.relink(index, session, pane_id, agent);
        let entry = &mut self.agents[index];
        apply(&mut entry.role, &patch.role);
        apply(&mut entry.project, &patch.project);
        apply(&mut entry.note, &patch.note);
        Ok(entry.clone())
    }

    pub fn unmanage(
        &mut self,
        session: Option<&str>,
        pane_id: Option<&str>,
    ) -> Option<ManagedAgent> {
        let index = self.find(session, pane_id)?;
        Some(self.agents.remove(index))
    }

    pub fn coordinator(&self) -> Option<&ManagedAgent> {
        self.agents.iter().find(|entry| entry.is_coordinator())
    }

    pub fn coordinator_index(&self) -> Option<usize> {
        self.agents.iter().position(ManagedAgent::is_coordinator)
    }

    /// Overwrite the keys of entry `index` (after a fresh coordinator launch,
    /// whose session differs so [`find`](Self::find) would refuse the pane,
    /// and after `pane.move` changed the pane id). `None` or an empty value
    /// keeps that key. Other entries still holding the same session or pane id
    /// are stale (a pane hosts one agent) and lose that key, so a lookup
    /// cannot land on them. `true` when anything changed.
    pub fn set_keys(&mut self, index: usize, session: Option<&str>, pane_id: Option<&str>) -> bool {
        if index >= self.agents.len() {
            return false;
        }
        let session = session.filter(|s| !s.is_empty());
        let pane_id = pane_id.filter(|p| !p.is_empty());
        let before = self.agents.clone();
        for (other, entry) in self.agents.iter_mut().enumerate() {
            if other == index {
                if let Some(session) = session {
                    entry.session = Some(session.to_string());
                }
                if let Some(pane_id) = pane_id {
                    entry.pane_id = Some(pane_id.to_string());
                }
                continue;
            }
            if session.is_some() && entry.session.as_deref() == session {
                entry.session = None;
            }
            if pane_id.is_some() && entry.pane_id.as_deref() == pane_id {
                entry.pane_id = None;
            }
        }
        self.agents != before
    }
}

/// The previous `managed.json`, kept by every [`update`] that changes it.
pub fn backup_path(dir: &Path) -> std::path::PathBuf {
    dir.join("managed.json.bak")
}

#[derive(Debug)]
pub enum LoadError {
    Corrupt(String),
    Io(io::Error),
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

/// Lock the registry, load it strictly, run `f`, and save when it changed
/// (keeping the previous file as `managed.json.bak`). A corrupt file is never
/// overwritten, and an `Err` from `f` saves nothing. This is the only way to
/// change `managed.json`: the lock serializes the MCP servers, the watcher
/// and the CLI, which all write it.
pub fn update<R>(
    dir: &Path,
    f: impl FnOnce(&mut Registry) -> Result<R, String>,
) -> Result<R, String> {
    let _lock = super::lock::exclusive(dir, super::REGISTRY_LOCK)
        .map_err(|err| format!("cannot lock the registry: {err}"))?;
    let mut registry = match load_strict(dir) {
        Ok(registry) => registry,
        Err(LoadError::Corrupt(err)) => {
            tracing::warn!("herdr+ registry is corrupt: {err}");
            return Err("managed.json is corrupt; fix or remove it".into());
        }
        Err(LoadError::Io(err)) => return Err(format!("cannot read managed.json: {err}")),
    };
    let before = registry.clone();
    let out = f(&mut registry)?;
    if registry != before {
        let path = registry_path(dir);
        if path.exists() {
            std::fs::copy(&path, backup_path(dir))
                .map_err(|err| format!("cannot back up managed.json: {err}"))?;
        }
        registry
            .save(dir)
            .map_err(|err| format!("cannot write managed.json: {err}"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(role: &str, project: &str) -> ManagePatch {
        ManagePatch {
            role: Some(role.into()),
            project: Some(project.into()),
            note: None,
        }
    }

    #[test]
    fn matches_by_session_first_and_follows_a_remapped_pane() {
        let mut registry = Registry::default();
        registry
            .manage(
                Some("s1"),
                Some("w1:p1"),
                Some("claude"),
                &patch("lead", "app"),
            )
            .unwrap();
        // After a restore the pane id changed; the session id still matches.
        let index = registry.find(Some("s1"), Some("w4:p9")).unwrap();
        assert!(registry.relink(index, Some("s1"), Some("w4:p9"), None));
        assert_eq!(registry.agents[0].pane_id.as_deref(), Some("w4:p9"));
        // The old pane now hosts another conversation: no match.
        assert_eq!(registry.find(Some("s2"), Some("w1:p1")), None);
        // A pane-only entry learns its session id on the first sighting.
        registry
            .manage(None, Some("w2:p2"), None, &patch("reviewer", ""))
            .unwrap();
        let index = registry.find(Some("s3"), Some("w2:p2")).unwrap();
        registry.relink(index, Some("s3"), Some("w2:p2"), Some("codex"));
        assert_eq!(registry.agents[1].session.as_deref(), Some("s3"));
        assert_eq!(registry.agents[1].project, None, "empty clears");
    }

    #[test]
    fn there_is_only_one_coordinator_and_patches_keep_unset_fields() {
        let mut registry = Registry::default();
        registry
            .manage(Some("c"), Some("w1:p1"), None, &patch(COORDINATOR_ROLE, ""))
            .unwrap();
        let err = registry
            .manage(Some("d"), Some("w1:p2"), None, &patch(COORDINATOR_ROLE, ""))
            .unwrap_err();
        assert!(err.contains("already the coordinator"));
        // Re-registering the holder is fine.
        registry
            .manage(
                Some("c"),
                Some("w1:p1"),
                None,
                &patch(COORDINATOR_ROLE, "herdr+"),
            )
            .unwrap();
        let kept = registry
            .manage(
                Some("c"),
                None,
                None,
                &ManagePatch {
                    note: Some("runs the dashboard".into()),
                    ..ManagePatch::default()
                },
            )
            .unwrap();
        assert_eq!(kept.role.as_deref(), Some(COORDINATOR_ROLE));
        assert_eq!(kept.project.as_deref(), Some("herdr+"));
        assert!(registry.coordinator().is_some());
        assert!(registry.unmanage(Some("c"), None).is_some());
        assert!(registry.coordinator().is_none());
    }

    #[test]
    fn round_trips_through_the_file() {
        let dir = super::super::test_dir("registry");
        let mut registry = Registry::default();
        registry
            .manage(
                Some("s"),
                Some("w1:p1"),
                Some("claude"),
                &patch("lead", "x"),
            )
            .unwrap();
        registry.save(&dir).unwrap();
        assert_eq!(Registry::load(&dir), registry);
        assert_eq!(Registry::load(&dir.join("missing")), Registry::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_keys_moves_lookups_to_the_new_keys() {
        let mut registry = Registry::default();
        registry
            .manage(
                Some("old"),
                Some("w1:p1"),
                Some("claude"),
                &patch(COORDINATOR_ROLE, "herdr+"),
            )
            .unwrap();
        // A stale pane-only entry that happens to hold the new pane id.
        registry
            .manage(None, Some("w3:p5"), None, &patch("reviewer", ""))
            .unwrap();
        let index = registry.coordinator_index().unwrap();
        // A fresh session in another pane: find refuses it before set_keys.
        assert_eq!(registry.find(Some("new"), Some("w1:p1")), None);
        assert!(registry.set_keys(index, Some("new"), Some("w3:p5")));
        assert_eq!(registry.find(Some("new"), Some("w3:p5")), Some(index));
        assert_eq!(registry.find(None, Some("w3:p5")), Some(index));
        assert_eq!(registry.find(Some("old"), Some("w1:p1")), None);
        assert_eq!(registry.agents[1].pane_id, None, "stale duplicate dropped");
        assert_eq!(registry.agents[index].agent.as_deref(), Some("claude"));
        // None keeps a key (a codex agent moved before its session is known).
        assert!(registry.set_keys(index, None, Some("w4:p1")));
        assert_eq!(registry.agents[index].session.as_deref(), Some("new"));
        assert!(!registry.set_keys(index, Some("new"), Some("w4:p1")));
        assert!(!registry.set_keys(9, Some("x"), None));
        assert_eq!(Registry::default().coordinator_index(), None);
    }

    #[test]
    fn update_serializes_concurrent_writers() {
        let dir = super::super::test_dir("registry-update");
        update(&dir, |registry| {
            registry.manage(Some("s"), None, None, &ManagePatch::default())
        })
        .unwrap();
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for _ in 0..200 {
                        update(&dir, |registry| {
                            let entry = &mut registry.agents[0];
                            let count: u64 = entry
                                .note
                                .as_deref()
                                .unwrap_or("0")
                                .parse()
                                .map_err(|err| format!("{err}"))?;
                            entry.note = Some((count + 1).to_string());
                            Ok(())
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(Registry::load(&dir).agents[0].note.as_deref(), Some("400"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_keeps_a_backup_and_never_overwrites_a_corrupt_file() {
        let dir = super::super::test_dir("registry-corrupt");
        // No change: nothing is written.
        assert_eq!(update(&dir, |_| Ok(1)), Ok(1));
        assert!(!registry_path(&dir).exists());
        update(&dir, |registry| {
            registry.manage(Some("a"), Some("w1:p1"), None, &patch("lead", ""))
        })
        .unwrap();
        assert!(!backup_path(&dir).exists(), "nothing to back up yet");
        let first = std::fs::read(registry_path(&dir)).unwrap();
        update(&dir, |registry| {
            registry.manage(Some("b"), Some("w1:p2"), None, &patch("rev", ""))
        })
        .unwrap();
        assert_eq!(std::fs::read(backup_path(&dir)).unwrap(), first);
        assert_eq!(load_strict(&dir).unwrap().agents.len(), 2);

        // A failing closure saves nothing.
        let second = std::fs::read(registry_path(&dir)).unwrap();
        let err = update(&dir, |registry| -> Result<(), String> {
            registry.agents.clear();
            Err("no".into())
        });
        assert_eq!(err, Err("no".into()));
        assert_eq!(std::fs::read(registry_path(&dir)).unwrap(), second);

        std::fs::write(registry_path(&dir), b"{ not json").unwrap();
        assert!(matches!(load_strict(&dir), Err(LoadError::Corrupt(_))));
        let err = update(&dir, |registry| {
            registry.agents.clear();
            Ok(())
        })
        .unwrap_err();
        assert!(err.contains("corrupt"), "{err}");
        assert_eq!(std::fs::read(registry_path(&dir)).unwrap(), b"{ not json");
        assert_eq!(std::fs::read(backup_path(&dir)).unwrap(), first);
        assert!(matches!(load_strict(&dir.join("missing")), Ok(r) if r == Registry::default()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
