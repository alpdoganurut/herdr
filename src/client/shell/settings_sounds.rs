//! The settings overlay's `sound` section: the on/off choice plus one row per
//! notification sound (finished, needs input, reminder). Enter (or a click)
//! on a sound row opens a picker in place of the section's list: `default`
//! and the sounds in /System/Library/Sounds. Moving through the picker plays
//! each sound once; Enter writes `ui.sound.done_path`, `request_path` or
//! `reminder_path` (`default` removes the key) and returns to the rows, Esc
//! returns without saving.

use super::*;

/// Where the pickers look for sounds (macOS system sounds). A missing
/// directory leaves only `default`.
pub(super) const SYSTEM_SOUNDS_DIR: &str = "/System/Library/Sounds";

/// The on/off rows before the sound rows.
pub(super) const ON_OFF_ROWS: usize = 2;

/// A sound the section can pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SoundTarget {
    Finished,
    NeedsInput,
    Reminder,
}

impl SoundTarget {
    pub(super) const ALL: [SoundTarget; 3] = [
        SoundTarget::Finished,
        SoundTarget::NeedsInput,
        SoundTarget::Reminder,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            SoundTarget::Finished => "finished",
            SoundTarget::NeedsInput => "needs input",
            SoundTarget::Reminder => "reminder",
        }
    }

    /// The `[ui.sound]` key the target writes.
    pub(super) fn key(self) -> &'static str {
        match self {
            SoundTarget::Finished => "done_path",
            SoundTarget::NeedsInput => "request_path",
            SoundTarget::Reminder => "reminder_path",
        }
    }

    /// What `default` plays for the target.
    fn fallback(self) -> crate::sound::Sound {
        match self {
            SoundTarget::NeedsInput => crate::sound::Sound::Request,
            SoundTarget::Finished | SoundTarget::Reminder => crate::sound::Sound::Done,
        }
    }

    fn index(self) -> usize {
        match self {
            SoundTarget::Finished => 0,
            SoundTarget::NeedsInput => 1,
            SoundTarget::Reminder => 2,
        }
    }
}

/// One picker row: its label and the file it writes (`None` = default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClientSoundChoice {
    pub(super) label: String,
    pub(super) path: Option<std::path::PathBuf>,
}

/// An open picker: the sound being chosen and its choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClientSoundPicker {
    pub(super) target: SoundTarget,
    pub(super) choices: Vec<ClientSoundChoice>,
}

/// The configured files, indexed like `SoundTarget::ALL`.
pub(super) fn sound_files(sound: &crate::config::SoundConfig) -> [Option<std::path::PathBuf>; 3] {
    [
        sound.done_path.clone(),
        sound.request_path.clone(),
        sound.reminder_path.clone(),
    ]
}

/// A sound file's display name: its name without the extension.
fn sound_name(path: &std::path::Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// `default`, then the files in `dir` sorted by name.
pub(super) fn sound_choices(dir: &std::path::Path) -> Vec<ClientSoundChoice> {
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.is_file())
                .collect()
        })
        .unwrap_or_default();
    files.sort_by_key(|path| sound_name(path).to_lowercase());
    std::iter::once(ClientSoundChoice {
        label: "default".into(),
        path: None,
    })
    .chain(files.into_iter().map(|path| ClientSoundChoice {
        label: sound_name(&path),
        path: Some(path),
    }))
    .collect()
}

impl ClientShellConfig {
    /// The row text for a sound target: "finished: Glass" / "finished: default".
    pub(super) fn sound_row_label(&self, target: SoundTarget) -> String {
        let name = self.sound_files[target.index()]
            .as_deref()
            .map_or_else(|| "default".to_string(), sound_name);
        format!("{}: {name}", target.label())
    }
}

impl ClientShellState {
    fn sound_picker(&self) -> Option<&ClientSoundPicker> {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
                section: ClientSettingsSection::Sound,
                sound_picker,
                ..
            })) => sound_picker.as_ref(),
            _ => None,
        }
    }

    /// Rows in the sound section: the picker's choices while one is open,
    /// else on, off and the three sound rows.
    pub(super) fn sound_section_rows(&self) -> usize {
        self.sound_picker()
            .map_or(ON_OFF_ROWS + SoundTarget::ALL.len(), |picker| {
                picker.choices.len()
            })
    }

    /// Enter on the sound section: on/off applies as before, a sound row opens
    /// its picker, a picker row writes the choice and closes the picker.
    pub(super) fn apply_sound_choice(&mut self, selected: usize, outcome: &mut ClientShellInput) {
        if let Some(picker) = self.sound_picker().cloned() {
            let Some(choice) = picker.choices.get(selected) else {
                return;
            };
            let Some(path) = choice
                .path
                .as_deref()
                .map(|path| path.to_str().ok_or(()))
                .transpose()
                .ok()
            else {
                self.set_endpoint_error("sound file path is not valid UTF-8");
                outcome.repaint = true;
                return;
            };
            let saved = self.save_settings_edit(
                crate::config::ConfigEdit::SoundFile {
                    key: picker.target.key(),
                    path,
                },
                outcome,
            );
            if saved {
                self.close_sound_picker();
            }
            return;
        }
        if selected < ON_OFF_ROWS {
            self.save_settings_edit(crate::config::ConfigEdit::Sound(selected == 0), outcome);
            return;
        }
        let Some(target) = SoundTarget::ALL.get(selected - ON_OFF_ROWS).copied() else {
            return;
        };
        let choices = sound_choices(&self.config.system_sounds_dir);
        let current = self.config.sound_files[target.index()].clone();
        let selected = choices
            .iter()
            .position(|choice| choice.path == current)
            .unwrap_or(0);
        if let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() {
            settings.sound_picker = Some(ClientSoundPicker { target, choices });
            settings.selected = selected;
        }
        outcome.repaint = true;
    }

    /// Leave an open picker for the section's rows, the cursor on its row.
    /// Returns whether a picker was open.
    pub(super) fn close_sound_picker(&mut self) -> bool {
        let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() else {
            return false;
        };
        let Some(picker) = settings.sound_picker.take() else {
            return false;
        };
        settings.selected = ON_OFF_ROWS + picker.target.index();
        true
    }

    /// Play the picker's selected choice once.
    pub(super) fn preview_sound_choice(&self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_ref() else {
            return;
        };
        let Some(picker) = self.sound_picker() else {
            return;
        };
        let Some(choice) = picker.choices.get(settings.selected) else {
            return;
        };
        outcome.actions.push(ClientShellAction::PreviewSound {
            path: choice.path.clone(),
            fallback: picker.target.fallback(),
        });
    }
}
