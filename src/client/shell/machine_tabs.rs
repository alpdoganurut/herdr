//! Machine tabs (fork): the `tabs` layout with several endpoints (Local and
//! the saved SSH machines). The sidebar shows ONE machine, the active
//! endpoint; everything it draws, folds and focuses belongs to it. The
//! `spaces` layout keeps upstream's machine list, the collapsed rail and the
//! phone layout list every machine as upstream does.
//!
//! Public ids repeat across servers (each starts at `w1:t1`), so whatever
//! the client keeps by id is kept per endpoint (`endpoint_set`) or dropped
//! on a switch (`forget_endpoint_view`).

use super::*;

/// How soon a background read asks again after it was cut off.
const POLL_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

/// `endpoint_id`'s ids of a fold preference kept as one set for Local and one
/// per saved machine (`collapsed_groups`, `expanded_runs`). Free function
/// over the fields: compose borrows other fields mutably.
pub(super) fn endpoint_set<'a>(
    endpoint_id: &ClientEndpointId,
    local: &'a HashSet<String>,
    remote: &'a HashMap<ClientEndpointId, HashSet<String>>,
    none: &'a HashSet<String>,
) -> &'a HashSet<String> {
    if endpoint_id.is_local() {
        local
    } else {
        remote.get(endpoint_id).unwrap_or(none)
    }
}

/// `endpoint_set`, to change it.
pub(super) fn endpoint_set_mut<'a>(
    endpoint_id: &ClientEndpointId,
    local: &'a mut HashSet<String>,
    remote: &'a mut HashMap<ClientEndpointId, HashSet<String>>,
) -> &'a mut HashSet<String> {
    if endpoint_id.is_local() {
        local
    } else {
        remote.entry(endpoint_id.clone()).or_default()
    }
}

/// Where the user was in a machine's list when another machine was shown.
#[derive(Debug)]
pub(super) struct MachineView {
    boot_id: String,
    focused_tab_id: Option<String>,
    list_scroll: usize,
    tab_history: super::tab_history::TabHistory,
    filter: super::sidebar_filter::SidebarFilter,
}

/// Whether the shown machine's rows are its last known ones: with several
/// machines, the active one is not online. One machine never looks stale
/// (its frame is the one without machine tabs).
pub(super) fn shows_cached_machine(
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
) -> bool {
    endpoints.len() > 1
        && endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == active_endpoint_id)
            .is_some_and(|endpoint| endpoint.status != ClientEndpointStatus::Online)
}

impl ClientShellState {
    pub(crate) fn tabs_layout(&self) -> bool {
        self.config.sidebar_layout == crate::config::SidebarLayoutConfig::Tabs
    }

    /// The expanded tabs sidebar draws the active machine (on a window wide
    /// enough for a sidebar).
    pub(super) fn tabs_sidebar_active(&self) -> bool {
        self.tabs_layout() && !self.sidebar_collapsed && self.snapshot.is_some()
    }

    /// `tabs_sidebar_active` while several machines share it, on a window
    /// that is not the phone layout (which lists every machine).
    pub(super) fn machine_sidebar_shown(&self) -> bool {
        self.multi_endpoint_active() && self.tabs_sidebar_active() && !self.mobile_layout_active()
    }

    /// The machines whose spaces the keys step through: the one the tabs
    /// sidebar shows, every machine where all are listed.
    pub(super) fn navigable_endpoints(&self) -> impl Iterator<Item = &ClientShellEndpoint> {
        let only = self
            .machine_sidebar_shown()
            .then_some(&self.active_endpoint_id);
        self.endpoints
            .iter()
            .filter(move |endpoint| only.is_none_or(|only| &endpoint.endpoint_id == only))
    }

    pub(super) fn active_collapsed_groups(&self) -> Option<&HashSet<String>> {
        self.collapsed_groups_for_endpoint(&self.active_endpoint_id)
    }

    pub(super) fn active_collapsed_groups_mut(&mut self) -> &mut HashSet<String> {
        endpoint_set_mut(
            &self.active_endpoint_id,
            &mut self.collapsed_groups,
            &mut self.remote_collapsed_groups,
        )
    }

    pub(super) fn active_expanded_runs_mut(&mut self) -> &mut HashSet<String> {
        endpoint_set_mut(
            &self.active_endpoint_id,
            &mut self.expanded_runs,
            &mut self.remote_expanded_runs,
        )
    }

    /// Focus a tab of the active machine. With several machines a Local row
    /// goes through the runtime, so a click on it while a switch is under
    /// way stays on Local (`focus_or_activate`).
    pub(super) fn focus_active_tab(&mut self, tab_id: String, outcome: &mut ClientShellInput) {
        if self.multi_endpoint_active() && self.tabs_layout() {
            let endpoint_id = self.active_endpoint_id.clone();
            self.focus_or_activate(endpoint_id, ClientEndpointFocusTarget::Tab(tab_id), outcome);
            return;
        }
        self.push_endpoint_method(
            crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget { tab_id }),
            outcome,
        );
    }

    /// Another machine is about to be shown in the `tabs` layout: the active
    /// one's list scroll, tab history and filters are put aside (an empty
    /// filter is left), its search field lets go of the keyboard.
    pub(super) fn remember_machine_view(&mut self) {
        if !self.tabs_layout() {
            return;
        }
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let mut tab_history = std::mem::take(&mut self.tab_history);
        // The jump's request went with the machine's connection lane.
        tab_history.drop_pending();
        let mut filter = std::mem::take(&mut self.sidebar_filter);
        filter.focused = false;
        self.machine_views.insert(
            self.active_endpoint_id.clone(),
            MachineView {
                boot_id: snapshot.boot_id.clone(),
                focused_tab_id: snapshot.focused_tab_id.clone(),
                list_scroll: self.agent_scroll,
                tab_history,
                filter,
            },
        );
    }

    /// The active machine is shown again (its snapshot is applied): its
    /// filters come back, and with the same server its list scroll and tab
    /// history. The list stays where it was while the same tab is focused,
    /// else it reveals the focused one from there.
    pub(super) fn restore_machine_view(&mut self) {
        let Some(view) = self.machine_views.remove(&self.active_endpoint_id) else {
            return;
        };
        if !self.tabs_layout() {
            return;
        }
        self.sidebar_filter = view.filter;
        self.sidebar_model.mark_dirty();
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        if snapshot.boot_id != view.boot_id {
            return;
        }
        self.agent_scroll = view.list_scroll;
        if snapshot.focused_tab_id == view.focused_tab_id {
            self.reveal_focused_workspace = false;
        }
        self.tab_history = view.tab_history;
        if let Some(tab_id) = snapshot.focused_tab_id.as_deref() {
            let tabs = &snapshot.tabs;
            self.tab_history
                .observe(tab_id, |id| tabs.iter().any(|tab| tab.tab_id == id));
        }
    }

    /// Another machine is shown from here on: what is keyed by the old one's
    /// ids, and the single-slot reads that described it, go (the next tick
    /// pulls them from the new one).
    pub(super) fn forget_endpoint_view(&mut self) {
        self.sidebar_hover = None;
        self.sidebar_reveal_tab = None;
        self.news = Default::default();
        self.coordinator = Default::default();
        self.browser = Default::default();
    }

    /// A push for `endpoint_id` was stored; returns whether to repaint. Only
    /// the active machine's show in the sidebar: another's cost neither a
    /// model rebuild nor a frame (a switch rebuilds the model).
    pub(super) fn endpoint_push_shown(&mut self, endpoint_id: &ClientEndpointId) -> bool {
        let shown = *endpoint_id == self.active_endpoint_id;
        if shown {
            self.sidebar_model.mark_dirty();
        }
        shown
    }

    /// A background read was cut off (a machine switch, a lost connection):
    /// the last answer stays and the read is due again `POLL_RETRY` from now
    /// (as if its period ended then). Returns whether `kind` was one.
    pub(super) fn retry_cancelled_poll(
        &mut self,
        kind: &PendingEndpointKind,
        now: std::time::Instant,
    ) -> bool {
        let due_again = |interval: std::time::Duration| {
            Some(
                now.checked_sub(interval.saturating_sub(POLL_RETRY))
                    .unwrap_or(now),
            )
        };
        match kind {
            PendingEndpointKind::NewsGet => {
                self.news.loading = false;
                self.news.last_pull = due_again(super::news::NEWS_REFRESH_INTERVAL);
            }
            PendingEndpointKind::BrowserGet => {
                self.browser.loading = false;
                self.browser.last_pull = due_again(self.browser_refresh_interval());
            }
            PendingEndpointKind::Coordinator(super::coordinator::CoordinatorRequestKind::Get) => {
                self.coordinator.get_not_sent();
                self.coordinator.last_pull =
                    due_again(super::coordinator::COORDINATOR_REFRESH_INTERVAL);
            }
            _ => return false,
        }
        true
    }
}
