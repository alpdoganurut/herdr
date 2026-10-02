//! The coordinator's shell glue (fork): turns what `coordinator.rs` and
//! `settings_coordinator.rs` decide (`CoordinatorRequest`,
//! `CoordinatorEffect`) into endpoint methods, focus changes, config writes
//! and notices, and routes `coordinator.*` replies back to them. The two
//! modules stay free of `ClientShellState`; everything that touches it is
//! here.

use super::coordinator::{
    CoordinatorEffect, CoordinatorMenuItem, CoordinatorRequest, CoordinatorRequestKind,
    CoordinatorRow,
};
use super::*;
use crate::api::schema::coordinator::{
    CoordinatorOpenDashboardParams, CoordinatorSetEnabledParams, CoordinatorSetModelParams,
    CoordinatorSetNotifyParams, CoordinatorSetWakeCapsParams, CoordinatorStartParams,
    CoordinatorWakeParams,
};
use crate::api::schema::{EmptyParams, Method};

/// The `Method` for a request. The TUI never sets `caller_pane`: absent
/// means the user.
fn coordinator_method(request: CoordinatorRequest) -> Method {
    match request {
        CoordinatorRequest::Get => Method::CoordinatorGet(EmptyParams::default()),
        CoordinatorRequest::Open => Method::CoordinatorOpen(EmptyParams::default()),
        CoordinatorRequest::OpenDashboard { open } => {
            Method::CoordinatorOpenDashboard(CoordinatorOpenDashboardParams { open })
        }
        CoordinatorRequest::Wake => Method::CoordinatorWake(CoordinatorWakeParams::default()),
        CoordinatorRequest::Start { resume } => Method::CoordinatorStart(CoordinatorStartParams {
            resume,
            caller_pane: None,
        }),
        CoordinatorRequest::SetEnabled(enabled) => {
            Method::CoordinatorSetEnabled(CoordinatorSetEnabledParams {
                enabled,
                caller_pane: None,
            })
        }
        CoordinatorRequest::SetWakeCaps { cap_hour, cap_day } => {
            Method::CoordinatorSetWakeCaps(CoordinatorSetWakeCapsParams {
                cap_hour,
                cap_day,
                caller_pane: None,
            })
        }
        CoordinatorRequest::SetModel(model) => {
            Method::CoordinatorSetModel(CoordinatorSetModelParams {
                model,
                caller_pane: None,
            })
        }
        CoordinatorRequest::SetNotify(enabled) => {
            Method::CoordinatorSetNotify(CoordinatorSetNotifyParams {
                enabled,
                caller_pane: None,
            })
        }
    }
}

impl ClientShellState {
    fn spaces_layout(&self) -> bool {
        self.config.sidebar_layout != crate::config::SidebarLayoutConfig::Tabs
    }

    /// The pinned row for the active snapshot, when there is one.
    pub(crate) fn coordinator_row(&self) -> Option<CoordinatorRow> {
        self.coordinator.row(self.snapshot.as_deref()?)
    }

    /// The coordinator tab while the snapshot lists it (left out of the
    /// `tabs` list and its keyboard numbering).
    pub(super) fn coordinator_pinned_tab_id(&self) -> Option<String> {
        let snapshot = self.snapshot.as_deref()?;
        self.coordinator.pinned_tab_id(snapshot).map(str::to_owned)
    }

    /// Send one request; `false` when it could not be sent.
    fn push_coordinator_request(
        &mut self,
        request: CoordinatorRequest,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let kind = request.kind();
        self.push_endpoint_method_with_kind(
            coordinator_method(request),
            PendingEndpointKind::Coordinator(kind),
            outcome,
        )
    }

    /// Carry out what a row click, a menu item or a settings row decided.
    fn apply_coordinator_effect(
        &mut self,
        effect: CoordinatorEffect,
        outcome: &mut ClientShellInput,
    ) {
        match effect {
            CoordinatorEffect::Nothing => {}
            CoordinatorEffect::Request(request) => {
                self.push_coordinator_request(request, outcome);
            }
            CoordinatorEffect::FocusTab(tab_id) => {
                self.push_endpoint_method(
                    Method::TabFocus(crate::api::schema::TabTarget { tab_id }),
                    outcome,
                );
            }
            CoordinatorEffect::OpenSettings => {
                self.open_settings_overlay();
                self.select_settings_section(ClientSettingsSection::Coordinator, outcome);
            }
            CoordinatorEffect::SwitchLayoutToTabs => {
                self.save_settings_edit(crate::config::ConfigEdit::SidebarLayoutTabs, outcome);
            }
            CoordinatorEffect::Refused(reason) => {
                self.set_endpoint_error(reason);
            }
        }
        outcome.repaint = true;
    }

    /// A left click on the row: focus the coordinator tab, or create it.
    pub(super) fn activate_coordinator_row(&mut self, outcome: &mut ClientShellInput) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let effect = self.coordinator.activate_row(snapshot);
        self.apply_coordinator_effect(effect, outcome);
    }

    /// Right-click on the row: Open dashboard, Focus, Wake now, Restart,
    /// Pause/Resume, Settings.
    pub(super) fn open_coordinator_context_menu(&mut self, x: u16, y: u16) {
        if self.coordinator_row().is_none() {
            return;
        }
        let items = self.coordinator.menu_items();
        if items.is_empty() {
            return;
        }
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Coordinator { items },
            x,
            y,
            highlighted: 0,
        }));
    }

    /// A coordinator menu item.
    pub(super) fn activate_coordinator_context_action(
        &mut self,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) {
        let ClientContextMenuAction::Coordinator(action) = action else {
            return;
        };
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let remote = !self.active_endpoint_id.is_local();
        let effect = self.coordinator.activate_menu(action, snapshot, remote);
        self.apply_coordinator_effect(effect, outcome);
    }

    /// The tick: pull `coordinator.get` when due.
    pub(crate) fn tick_coordinator(
        &mut self,
        now: std::time::Instant,
        outcome: &mut ClientShellInput,
    ) {
        let advertised =
            self.supports_endpoint_method(&Method::CoordinatorGet(EmptyParams::default()));
        let online = self.endpoint_is_online(&self.active_endpoint_id);
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let (repaint, request) = self.coordinator.tick(snapshot, advertised, online, now);
        outcome.repaint |= repaint;
        if let Some(request) = request {
            if !self.push_coordinator_request(request, outcome) {
                self.coordinator.get_not_sent();
            }
        }
    }

    /// `coordinator.*` replies: every method answers with the read model.
    /// Errors already raised the generic notice.
    pub(super) fn handle_coordinator_endpoint_result(
        &mut self,
        kind: CoordinatorRequestKind,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let reply = match result {
            Ok(crate::api::schema::ResponseResult::CoordinatorGet { info }) => Some(info),
            Ok(_) => {
                self.set_endpoint_error("endpoint returned an unexpected coordinator result");
                None
            }
            Err(_) => None,
        };
        let snapshot = self.snapshot.as_deref();
        if let Some(notice) = self.coordinator.on_reply(kind, reply, snapshot) {
            self.set_endpoint_error(notice);
        }
        self.sync_coordinator_settings();
        (true, Vec::new())
    }

    // ----- the settings section --------------------------------------------

    fn coordinator_settings(
        &self,
    ) -> Option<&super::settings_coordinator::ClientCoordinatorSettings> {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::Settings(settings)) => Some(&settings.coordinator),
            _ => None,
        }
    }

    /// Rows in the section (the picker's while one is open).
    pub(super) fn coordinator_section_rows(&self) -> usize {
        let spaces = self.spaces_layout();
        self.coordinator_settings()
            .map_or(0, |settings| settings.row_count(spaces))
    }

    /// Entering the section: show what the shell knows and pull a fresh
    /// record on the next tick.
    pub(super) fn enter_coordinator_section(&mut self) {
        let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() else {
            return;
        };
        settings.coordinator.enter(&self.coordinator);
        self.coordinator.refresh();
    }

    /// A reply reaches the open section.
    fn sync_coordinator_settings(&mut self) {
        let spaces = self.spaces_layout();
        let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() else {
            return;
        };
        let selected = settings
            .coordinator
            .sync(&self.coordinator, settings.selected, spaces);
        if settings.section == ClientSettingsSection::Coordinator {
            settings.selected = selected;
        }
    }

    /// Enter or a click on the section's current row.
    pub(super) fn apply_coordinator_choice(
        &mut self,
        selected: usize,
        outcome: &mut ClientShellInput,
    ) {
        let spaces = self.spaces_layout();
        let remote = !self.active_endpoint_id.is_local();
        let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() else {
            return;
        };
        let result = settings.coordinator.apply(selected, spaces, remote);
        if let Some(select) = result.select {
            settings.selected = select;
        }
        self.apply_coordinator_effect(result.effect, outcome);
    }

    /// Esc while a picker is open goes back to the rows.
    pub(super) fn close_coordinator_picker(&mut self) -> bool {
        let spaces = self.spaces_layout();
        let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() else {
            return false;
        };
        if settings.section != ClientSettingsSection::Coordinator {
            return false;
        }
        match settings.coordinator.close_picker(spaces) {
            Some(select) => {
                settings.selected = select;
                true
            }
            None => false,
        }
    }

    /// A click on a row acts at once (as in the news section).
    pub(super) fn coordinator_click_applies(&self) -> bool {
        matches!(
            self.overlay,
            Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
                section: ClientSettingsSection::Coordinator,
                ..
            }))
        )
    }
}

/// The context menu's items for the coordinator target.
pub(super) fn coordinator_menu_items(items: &[CoordinatorMenuItem]) -> Vec<ClientContextMenuItem> {
    items
        .iter()
        .map(|item| ClientContextMenuItem {
            label: item.label,
            action: ClientContextMenuAction::Coordinator(item.action),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_request_maps_to_its_named_method() {
        for request in [
            CoordinatorRequest::Get,
            CoordinatorRequest::Open,
            CoordinatorRequest::OpenDashboard { open: false },
            CoordinatorRequest::Wake,
            CoordinatorRequest::Start { resume: true },
            CoordinatorRequest::SetEnabled(true),
            CoordinatorRequest::SetWakeCaps {
                cap_hour: 4,
                cap_day: 20,
            },
            CoordinatorRequest::SetModel(None),
            CoordinatorRequest::SetNotify(false),
        ] {
            let name = request.method_name();
            let method = coordinator_method(request);
            assert_eq!(crate::api::api_method_name(&method), name);
            assert!(crate::server::client_commands::supports_client_shell_method(&method));
        }
    }
}
