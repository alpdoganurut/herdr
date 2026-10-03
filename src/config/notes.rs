//! `[notes]`: per-session notes and checkpoints behind the info pane (fork).
//!
//! On by default. With `enabled = false` every `notes.*` and `checkpoints.*`
//! method answers `notes_disabled`, and nothing is read or written under
//! `<config_dir>/notes/`. `auto_checkpoints = false` keeps the notes but
//! stops herdr's own checkpoints (`crate::notes::auto`).

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct NotesConfig {
    /// Serve the `notes.*` and `checkpoints.*` methods (the info pane, the
    /// `herdr notes` / `herdr checkpoint` CLI and the agents' notes tools).
    /// Default: true.
    pub enabled: bool,
    /// herdr adds checkpoints on its own (tagged `auto`): a bookmark for
    /// each prompt you send an agent, a milestone for commits made in the
    /// pane's repository during a turn, a failure when an agent stays
    /// blocked 10 minutes or more or exits mid-turn. Default: true.
    pub auto_checkpoints: bool,
}

impl Default for NotesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_checkpoints: true,
        }
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
        assert!(config.notes.auto_checkpoints);
        let config: crate::config::Config =
            toml::from_str("[notes]\nauto_checkpoints = false\n").unwrap();
        assert!(config.notes.enabled && !config.notes.auto_checkpoints);
    }
}
