//! The settings overlay's `reminders` section: below the interval choices, a
//! `daily at HH:MM` row for `ui.daily_reminder_time`. Enter (or a click) on
//! it opens a picker in place of the section's list: 24-hour times in
//! 30-minute steps, 00:00 through 23:30, with a configured time off that grid
//! first as `custom: HH:MM`. The configured time is checked and selected on
//! open. Enter writes `ui.daily_reminder_time` and returns to the rows, Esc
//! returns without saving.

use super::*;

/// The picker's step between listed times.
const STEP_MINUTES: u32 = 30;

/// One picker row: its label and the time it writes, in minutes past
/// midnight.
pub(super) type ClientDailyTimeChoice = (String, u32);

/// The picker's rows for a configured time: `custom: HH:MM` first when it is
/// off the 30-minute grid, then every step of the day.
pub(super) fn daily_time_choices(configured: u32) -> Vec<ClientDailyTimeChoice> {
    let custom = (!configured.is_multiple_of(STEP_MINUTES)).then(|| {
        (
            format!("custom: {}", crate::config::format_time_of_day(configured)),
            configured,
        )
    });
    custom
        .into_iter()
        .chain(
            (0..24 * 60)
                .step_by(STEP_MINUTES as usize)
                .map(|minutes| (crate::config::format_time_of_day(minutes), minutes)),
        )
        .collect()
}

/// The reminders section's row text for the daily time.
pub(super) fn daily_time_row_label(configured: u32) -> String {
    format!("daily at {}", crate::config::format_time_of_day(configured))
}

impl ClientShellState {
    /// Rows in the reminders section: the picker's while it is open, else the
    /// interval choices and the daily time row.
    pub(super) fn reminders_section_rows(&self) -> usize {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
                daily_time_picker: Some(choices),
                ..
            })) => choices.len(),
            Some(ClientShellOverlay::Settings(settings)) => {
                super::idle_reminders::reminder_choices(settings.idle_reminder_minutes).len() + 1
            }
            _ => 0,
        }
    }

    /// The daily time row's index in the reminders section.
    fn daily_time_row(&self) -> Option<usize> {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::Settings(settings)) => {
                Some(super::idle_reminders::reminder_choices(settings.idle_reminder_minutes).len())
            }
            _ => None,
        }
    }

    /// Enter on the reminders section when it is the daily time row or the
    /// picker: open the picker, or write the picked time and close it.
    /// Returns whether it handled the choice (else it is an interval).
    pub(super) fn apply_daily_time_choice(
        &mut self,
        selected: usize,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let picker = match self.overlay.as_ref() {
            Some(ClientShellOverlay::Settings(settings)) => settings.daily_time_picker.clone(),
            _ => return false,
        };
        if let Some(choices) = picker {
            let Some((_, minutes)) = choices.get(selected).cloned() else {
                return true;
            };
            if self.save_settings_edit(
                crate::config::ConfigEdit::DailyReminderTime(minutes),
                outcome,
            ) {
                self.close_daily_time_picker();
            }
            return true;
        }
        if self.daily_time_row() != Some(selected) {
            return false;
        }
        let configured = self.config.daily_reminder_minutes;
        let choices = daily_time_choices(configured);
        let selected = choices
            .iter()
            .position(|(_, minutes)| *minutes == configured)
            .unwrap_or(0);
        if let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() {
            settings.daily_time_picker = Some(choices);
            settings.selected = selected;
        }
        outcome.repaint = true;
        true
    }

    /// Leave an open picker for the section's rows, the cursor on the daily
    /// time row. Returns whether a picker was open.
    pub(super) fn close_daily_time_picker(&mut self) -> bool {
        let row = self.daily_time_row();
        let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() else {
            return false;
        };
        if settings.daily_time_picker.take().is_none() {
            return false;
        }
        settings.selected = row.unwrap_or(0);
        true
    }

    /// Whether a click on the reminders section's current row acts at once:
    /// the daily time row (opens the picker) and the picker's rows (pick).
    pub(super) fn reminders_click_applies(&self) -> bool {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::Settings(
                settings @ ClientSettingsOverlay {
                    section: ClientSettingsSection::Reminders,
                    ..
                },
            )) => {
                settings.daily_time_picker.is_some()
                    || self.daily_time_row() == Some(settings.selected)
            }
            _ => false,
        }
    }
}
