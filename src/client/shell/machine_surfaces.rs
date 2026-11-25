//! Machine tabs (fork): the surfaces beside the sidebar with several
//! endpoints (Local and the saved SSH machines). Toasts, cards and reminders
//! name their machine and stay bound to it; what only means something on one
//! machine (its News and coordinator tabs, the coordinator's dashboard, the
//! settings of its server) says which. With one endpoint nothing here shows.
//!
//! Requests reach the active endpoint only, so what the user did to another
//! machine's card waits here until that machine is shown (`tick_machines`).

use super::*;
use crate::api::schema::{AgentNoticeDismissParams, EmptyParams, Method};

/// The News and coordinator tabs a machine last named (`news.get`,
/// `coordinator.*`), kept while another machine is shown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct FixedTabs {
    boot_id: String,
    news: Option<String>,
    coordinator: Option<String>,
}

/// `section` changes this client (its own config file); the others belong to
/// the selected machine's server.
fn client_settings(section: ClientSettingsSection) -> bool {
    use ClientSettingsSection as Section;
    match section {
        Section::Theme
        | Section::Indicators
        | Section::Sound
        | Section::Toast
        | Section::Reminders => true,
        Section::Integrations
        | Section::Backups
        | Section::ClosedSessions
        | Section::News
        | Section::Browser
        | Section::Coordinator
        | Section::Agents => false,
    }
}

/// Whose settings `section` shows, with several machines: the settings
/// panel's title line. `machine` is the selected one.
pub(super) fn settings_scope(section: ClientSettingsSection, machine: &str) -> String {
    if client_settings(section) {
        "this client, for every machine".into()
    } else {
        format!("on {machine}")
    }
}

/// `host:port` of a web URL.
fn authority(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

/// The port of a URL on the loopback interface.
fn loopback_port(url: &str) -> Option<&str> {
    let (host, port) = authority(url).rsplit_once(':')?;
    matches!(host, "127.0.0.1" | "localhost" | "[::1]").then_some(port)
}

/// A URL's address without its scheme: shown where a click on a link would
/// open the same port on the wrong machine.
pub(super) fn plain_address(url: &str) -> &str {
    authority(url)
}

impl ClientShellState {
    /// The machine's name where it has to be told apart: with several
    /// endpoints, Local included.
    pub(super) fn machine_name(&self, endpoint_id: &ClientEndpointId) -> Option<&str> {
        if !self.multi_endpoint_active() {
            return None;
        }
        self.endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .map(|endpoint| endpoint.label.as_str())
    }

    /// `text`, led by its machine's name with several endpoints.
    pub(super) fn name_machine(&self, endpoint_id: &ClientEndpointId, text: String) -> String {
        match self.machine_name(endpoint_id) {
            Some(machine) => format!("{machine} · {text}"),
            None => text,
        }
    }

    /// The user acted on something of a machine that is not reachable. Their
    /// click is answered every time, not once per server boot.
    pub(super) fn say_machine_unavailable(
        &mut self,
        endpoint_id: &ClientEndpointId,
        outcome: &mut ClientShellInput,
    ) {
        let message = format!("{} is unavailable", self.endpoint_label(endpoint_id));
        self.endpoint_notice_seen
            .retain(|seen| seen.code != message);
        self.receive_endpoint_unavailable(message);
        outcome.repaint = true;
    }

    /// Runs with every accepted snapshot of the active endpoint: another
    /// machine shown takes the notes dock and its toasts along.
    pub(super) fn note_shown_machine(&mut self) {
        self.sync_info_dock_endpoint();
        if self.shown_endpoint_id == self.active_endpoint_id {
            return;
        }
        self.shown_endpoint_id = self.active_endpoint_id.clone();
        // Its focused tab is in view from here on, without a new snapshot.
        let endpoint_id = self.active_endpoint_id.clone();
        self.prune_sticky_notifications(&endpoint_id);
    }

    /// The machines whose reminders wait: not online, so nothing says the
    /// agent still waits and a click could not go there. Empty with one
    /// machine.
    pub(super) fn reminders_held(&self) -> Vec<ClientEndpointId> {
        if !self.multi_endpoint_active() {
            return Vec::new();
        }
        self.endpoints
            .iter()
            .filter(|endpoint| endpoint.status != ClientEndpointStatus::Online)
            .map(|endpoint| endpoint.endpoint_id.clone())
            .collect()
    }

    /// After a `news.*` or `coordinator.*` reply of the active endpoint.
    pub(super) fn remember_fixed_tabs(&mut self) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let fixed = self
            .fixed_tabs
            .entry(self.active_endpoint_id.clone())
            .or_default();
        if fixed.boot_id != snapshot.boot_id {
            *fixed = FixedTabs {
                boot_id: snapshot.boot_id.clone(),
                ..FixedTabs::default()
            };
        }
        if let Some(info) = self.news.info.as_ref() {
            fixed.news = info.tab_id.clone();
        }
        if let Some(info) = self.coordinator.info.as_ref() {
            fixed.coordinator = info.tab_id.clone();
        }
    }

    /// Whether `tab_id` is the News or coordinator tab `endpoint_id` last
    /// named.
    pub(super) fn is_remembered_fixed_tab(
        &self,
        endpoint_id: &ClientEndpointId,
        tab_id: &str,
    ) -> bool {
        self.remembered_fixed_tabs(endpoint_id)
            .is_some_and(|(fixed, _)| {
                fixed.news.as_deref() == Some(tab_id)
                    || fixed.coordinator.as_deref() == Some(tab_id)
            })
    }

    /// The same for a machine that is not shown. One that was never shown in
    /// this session named none, and one can open its desk while another is
    /// shown: there the tab with the label its server gives it counts.
    pub(super) fn is_fixed_tab_elsewhere(
        &self,
        endpoint_id: &ClientEndpointId,
        tab: &crate::protocol::ClientShellTab,
    ) -> bool {
        if self.is_remembered_fixed_tab(endpoint_id, &tab.tab_id) {
            return true;
        }
        if !tab.custom_label {
            return false;
        }
        let label = tab.label.as_str();
        if label != super::news::NEWS_ROW_LABEL
            && label != super::coordinator::COORDINATOR_ROW_LABEL
        {
            return false;
        }
        let Some((fixed, snapshot)) = self.remembered_fixed_tabs(endpoint_id) else {
            return true;
        };
        let named = if label == super::news::NEWS_ROW_LABEL {
            fixed.news.as_deref()
        } else {
            fixed.coordinator.as_deref()
        };
        // the one it named is still there: this tab only shares its label
        !named.is_some_and(|named| snapshot.tabs.iter().any(|tab| tab.tab_id == named))
    }

    /// What `endpoint_id` last named, while it is still the same server.
    fn remembered_fixed_tabs(
        &self,
        endpoint_id: &ClientEndpointId,
    ) -> Option<(&FixedTabs, &ClientShellSnapshot)> {
        let fixed = self.fixed_tabs.get(endpoint_id)?;
        let snapshot = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)?
            .snapshot
            .as_deref()?;
        (snapshot.boot_id == fixed.boot_id).then_some((fixed, snapshot))
    }

    /// Whether `url` is the dashboard of the coordinator on the selected SSH
    /// machine: that machine's loopback, so here the port is another's.
    pub(super) fn is_remote_dashboard(&self, url: &str) -> bool {
        if self.active_endpoint_id.is_local() {
            return false;
        }
        let Some(dashboard) = self
            .coordinator
            .info
            .as_ref()
            .and_then(|info| info.dashboard_url.as_deref())
        else {
            return false;
        };
        loopback_port(url).is_some_and(|port| loopback_port(dashboard) == Some(port))
    }

    /// The tick: what waited for the machine now shown.
    pub(crate) fn tick_machines(&mut self, outcome: &mut ClientShellInput) {
        if !self.endpoint_is_online(&self.active_endpoint_id) || self.snapshot.is_none() {
            return;
        }
        if self.local_config_reload_due && self.active_endpoint_id.is_local() {
            self.local_config_reload_due = false;
            self.push_endpoint_method_with_kind(
                Method::ServerReloadConfig(EmptyParams::default()),
                PendingEndpointKind::ReloadConfig,
                outcome,
            );
        }
        let unsent = self
            .agent_cards
            .get_mut(&self.active_endpoint_id)
            .map(super::agent_cards::AgentCardsState::take_unsent)
            .unwrap_or_default();
        if !unsent.is_empty() {
            let method = Method::AgentNoticeDismiss(AgentNoticeDismissParams {
                ids: unsent,
                all: false,
            });
            if self.supports_endpoint_method(&method) {
                self.push_endpoint_method(method, outcome);
            }
        }
    }
}
