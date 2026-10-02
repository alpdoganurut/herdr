//! `[notes]`: per-session notes and checkpoints behind the info pane (fork).
//!
//! On by default. With `enabled = false` every `notes.*` and `checkpoints.*`
//! method answers `notes_disabled`, and nothing is read or written under
//! `<config_dir>/notes/`.

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct NotesConfig {
    /// Serve the `notes.*` and `checkpoints.*` methods (the info pane, the
    /// `herdr notes` / `herdr checkpoint` CLI and the agents' notes tools).
    /// Default: true.
    pub enabled: bool,
}

impl Default for NotesConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[cfg(test)]
mod tests {
    use super::NotesConfig;

    #[test]
    fn notes_are_enabled_by_default_and_can_be_turned_off() {
        assert!(NotesConfig::default().enabled);
        let config: crate::config::Config = toml::from_str("[notes]\nenabled = false\n").unwrap();
        assert!(!config.notes.enabled);
        let config: crate::config::Config = toml::from_str("").unwrap();
        assert!(config.notes.enabled);
    }
}
