//! The remote browser on the client (fork): a saved machine whose server
//! takes this Mac's browser (`browser-link.v1`) has a helper on this Mac
//! (`herdr browser-link <machine>`) that runs that machine's companion. The
//! client talks to it on a socket of its own (`client/browser_link`); this
//! module is the shell's side of that and nothing else: what the helpers
//! last said, and what the shell asks of them, kept as plain data.
//!
//! The dock follows a machine's helper as it follows the Local server's
//! companion: the dock is offered while the helper is attached, its
//! placement goes to the helper (never to the machine's server, which never
//! learns this Mac's pids), and the companion's state and its report on the
//! placement are the helper's. On a machine switch the machine left gets
//! `active: false` before the machine shown gets its placement.
//!
//! Per layout the dock asks one map lookup of this (a Local endpoint none);
//! nothing here locks, reads a socket or allocates on that path.

use std::collections::HashMap;

use super::*;
use crate::api::schema::{
    BrowserCompanionAction, BrowserCompanionAppInfo, BrowserPlacementSpec, BrowserPlacementStatus,
};
use crate::server::headless::placement_action::PlacementActionPayload;
use crate::server::headless::view_cycle::ViewCyclePayload;

/// The dock and Browser view over `herdr --remote` (the browser belongs to
/// the Mac in front of the user, which that connection does not reach).
pub(crate) const REMOTE_CLIENT_REASON: &str =
    "The browser is not available over herdr --remote; add the machine (herdr machine add).";
/// A saved machine without an attached helper.
pub(crate) const NOT_LINKED_REASON: &str = "This machine's browser is not linked: it needs herdr+ on the machine and [browser] remote_machines on this Mac.";

/// A machine's browser as its helper last said.
// only the unix client loop (`client/browser_link/links.rs`) reads the helpers
#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LinkedBrowser {
    /// `connecting`, `linked`, `reconnecting`, `in_use` (another Mac has
    /// it), `off` (the machine takes no link) or `failed`.
    pub(crate) link: String,
    /// `in_use`: which Mac.
    pub(crate) owner: Option<String>,
    /// Why, for the user.
    pub(crate) text: Option<String>,
    pub(crate) companion: BrowserCompanionAppInfo,
    pub(crate) reports: Vec<BrowserPlacementStatus>,
}

/// A placement as a helper was last told.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LinkPlacement {
    pub(crate) spec: Option<BrowserPlacementSpec>,
    /// This client shows the machine now.
    pub(crate) active: bool,
    /// The terminal has the user's focus.
    pub(crate) focused: bool,
}

/// What the shell asks of a machine's helper, in order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LinkIntent {
    Placement {
        endpoint: ClientEndpointId,
        placement: LinkPlacement,
    },
    Companion {
        endpoint: ClientEndpointId,
        action: BrowserCompanionAction,
    },
}

/// The helpers' facts and the shell's asks (the one field
/// `ClientShellState::browser_link`).
#[derive(Debug, Default)]
pub(crate) struct ClientBrowserLinkState {
    /// This client is `herdr --remote`: the dock is never offered.
    pub(crate) remote_client: bool,
    /// The machines whose helper is attached, with what it said.
    attached: HashMap<ClientEndpointId, LinkedBrowser>,
    /// Why a machine has no attached helper (another herdr+ version, a
    /// helper that did not start), for the dock's notice.
    notices: HashMap<ClientEndpointId, String>,
    /// What each helper was told last.
    told: HashMap<ClientEndpointId, LinkPlacement>,
    /// The machine whose helper was last told it is shown.
    shown: Option<ClientEndpointId>,
    // read only by the unix client loop
    #[cfg_attr(not(unix), allow(dead_code))]
    intents: Vec<LinkIntent>,
}

impl ClientBrowserLinkState {
    pub(crate) fn new(remote_client: bool) -> Self {
        Self {
            remote_client,
            ..Self::default()
        }
    }

    /// The attached helper's facts of `endpoint` (a map lookup).
    pub(crate) fn attached(&self, endpoint: &ClientEndpointId) -> Option<&LinkedBrowser> {
        if self.attached.is_empty() {
            return None;
        }
        self.attached.get(endpoint)
    }

    pub(crate) fn notice(&self, endpoint: &ClientEndpointId) -> Option<&str> {
        self.notices.get(endpoint).map(String::as_str)
    }

    /// What `endpoint`'s helper says now (`None`: no helper attached);
    /// whether it changed.
    #[cfg_attr(not(unix), allow(dead_code))] // the unix client loop's
    pub(crate) fn set(
        &mut self,
        endpoint: &ClientEndpointId,
        browser: Option<LinkedBrowser>,
        notice: Option<String>,
    ) -> bool {
        let notice_changed = match notice {
            Some(notice) => self.notices.insert(endpoint.clone(), notice.clone()) != Some(notice),
            None => self.notices.remove(endpoint).is_some(),
        };
        let changed = match browser {
            Some(browser) => {
                if self.attached.get(endpoint) == Some(&browser) {
                    false
                } else {
                    self.attached.insert(endpoint.clone(), browser);
                    true
                }
            }
            None => {
                // a new helper is told everything again
                self.told.remove(endpoint);
                if self.shown.as_ref() == Some(endpoint) {
                    self.shown = None;
                }
                self.attached.remove(endpoint).is_some()
            }
        };
        changed || notice_changed
    }

    /// `endpoint`'s placement, unless the helper has it already; whether it
    /// was queued.
    pub(crate) fn place(&mut self, endpoint: &ClientEndpointId, placement: LinkPlacement) -> bool {
        if self.told.get(endpoint) == Some(&placement) {
            return false;
        }
        if placement.active {
            self.shown = Some(endpoint.clone());
        }
        self.told.insert(endpoint.clone(), placement.clone());
        self.intents.push(LinkIntent::Placement {
            endpoint: endpoint.clone(),
            placement,
        });
        true
    }

    /// What `endpoint`'s helper was told last.
    pub(crate) fn told(&self, endpoint: &ClientEndpointId) -> Option<&LinkPlacement> {
        self.told.get(endpoint)
    }

    /// The shown machine is no longer `active`: its helper hides the
    /// browser (`active: false`, the last area kept) before anything else
    /// is told. Whether it was queued.
    pub(crate) fn leave(&mut self, active: &ClientEndpointId) -> bool {
        let Some(shown) = self.shown.take_if(|shown| shown != active) else {
            return false;
        };
        let Some(last) = self.told.get(&shown).cloned() else {
            return false;
        };
        self.place(
            &shown,
            LinkPlacement {
                active: false,
                ..last
            },
        )
    }

    pub(crate) fn ask(&mut self, endpoint: &ClientEndpointId, action: BrowserCompanionAction) {
        self.intents.push(LinkIntent::Companion {
            endpoint: endpoint.clone(),
            action,
        });
    }

    #[cfg_attr(not(unix), allow(dead_code))] // the unix client loop's
    pub(crate) fn take_intents(&mut self) -> Vec<LinkIntent> {
        std::mem::take(&mut self.intents)
    }
}

#[cfg_attr(not(unix), allow(dead_code))] // the unix client loop's
fn absorb(into: &mut ClientShellInput, from: ClientShellInput) {
    into.detach |= from.detach;
    into.repaint |= from.repaint;
    into.resize |= from.resize;
    into.query_host_appearance |= from.query_host_appearance;
    into.query_host_theme |= from.query_host_theme;
    into.requests.extend(from.requests);
    into.actions.extend(from.actions);
}

impl ClientShellState {
    /// The active endpoint's attached helper (a saved machine's), `None`
    /// on Local.
    pub(crate) fn browser_link_active(&self) -> Option<&LinkedBrowser> {
        if self.active_endpoint_id.is_local() {
            return None;
        }
        self.browser_link.attached(&self.active_endpoint_id)
    }

    /// The direct keys of `keys.cycle_browser`, for the helpers' leases.
    #[cfg_attr(not(unix), allow(dead_code))] // the unix client loop's
    pub(crate) fn browser_link_cycle_keys(&self) -> Vec<String> {
        crate::browser::hub::view_cycle_key_labels(&self.config.keybinds.keybinds.cycle_browser)
    }

    /// What `endpoint`'s helper said since the last look: its state (`None`
    /// while none is attached), why not, and the companion's asks for this
    /// client, taken as the server's relays are.
    #[cfg_attr(not(unix), allow(dead_code))] // the unix client loop's
    pub(crate) fn receive_browser_link(
        &mut self,
        endpoint: &ClientEndpointId,
        browser: Option<LinkedBrowser>,
        notice: Option<String>,
        actions: Vec<PlacementActionPayload>,
        cycles: Vec<ViewCyclePayload>,
        outcome: &mut ClientShellInput,
    ) {
        let active = *endpoint == self.active_endpoint_id;
        let size_before = active
            .then(|| {
                self.last_composed_size
                    .map(|(cols, rows)| self.surface_size(cols, rows))
            })
            .flatten();
        if self.browser_link.set(endpoint, browser, notice) && active {
            outcome.repaint = true;
            let size_after = self
                .last_composed_size
                .map(|(cols, rows)| self.surface_size(cols, rows));
            if size_before != size_after {
                outcome.resize = true;
            }
        }
        for action in actions {
            let taken = self.receive_browser_placement_action(endpoint, action);
            absorb(outcome, taken);
        }
        for cycle in cycles {
            let taken = self.receive_browser_view_cycle(endpoint, cycle);
            absorb(outcome, taken);
        }
    }

    /// What the helpers are to be told now, in order: the machine left
    /// first.
    #[cfg_attr(not(unix), allow(dead_code))] // the unix client loop's
    pub(crate) fn take_browser_link_intents(&mut self) -> Vec<LinkIntent> {
        if self.browser_link.leave(&self.active_endpoint_id) {
            // what this endpoint publishes waits a gap behind the hide
            self.browser_dock_wait_for_link();
        }
        self.browser_link.take_intents()
    }
}

#[cfg(test)]
impl ClientShellState {
    /// Tests outside the shell: the machine shown.
    pub(crate) fn show_endpoint_for_test(&mut self, endpoint: ClientEndpointId) {
        self.active_endpoint_id = endpoint;
    }

    /// Tests outside the shell: a placement for `endpoint`'s helper.
    pub(crate) fn place_on_link_for_test(
        &mut self,
        endpoint: &ClientEndpointId,
        placement: LinkPlacement,
    ) {
        self.browser_link.place(endpoint, placement);
    }
}
