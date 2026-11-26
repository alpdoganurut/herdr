//! The client loop's browser helpers: one [`LinkClient`] per saved machine
//! whose server takes this Mac's browser (`browser-link.v1`), while
//! `[browser] remote_machines` is on. It carries the shell's asks to the
//! helpers and the helpers' news into the shell; the shell's side
//! (`client/shell/browser_link.rs`) is plain data.
//!
//! A machine's client starts once its server advertised the capability on
//! a connection, and stays while the machine is saved and enabled (a
//! dropped connection does not end it: the helper reconnects by itself, and
//! its companion keeps its pages). A server that answers again without the
//! capability ends it.

use std::collections::{HashMap, HashSet};
use std::io;

use super::client::{CompanionAction, HelperNews, LinkClient, LinkClientConfig, MachineFacts};
use crate::api::schema::BrowserCompanionAction;
use crate::browser::link_proto::LINK_CAPABILITY;
use crate::client::endpoint::{ClientEndpointId, EndpointRegistry, ProfileId, SavedSshEndpoint};
use crate::client::shell::{ClientShellInput, ClientShellState, LinkIntent, LinkedBrowser};

/// Version 1 starts helpers on macOS only: the companion's key table and
/// the proxy's switches were measured there.
pub(crate) const STARTS_HELPERS: bool = cfg!(target_os = "macos");

/// Starts the client of one machine's helper.
pub(crate) type Starter = Box<dyn Fn(MachineFacts, Vec<String>) -> io::Result<LinkClient>>;

struct Running {
    facts: MachineFacts,
    client: LinkClient,
}

/// The helpers' clients of this client process.
pub(crate) struct ShellLinks {
    enabled: bool,
    start: Starter,
    running: HashMap<ProfileId, Running>,
    /// The machines whose server advertised the capability.
    advertised: HashSet<ProfileId>,
    /// The machines a start failed for, not tried again this process.
    failed: HashSet<ProfileId>,
}

fn facts(machine: &SavedSshEndpoint) -> MachineFacts {
    MachineFacts {
        id: machine.id.as_str().to_owned(),
        label: machine.label.clone(),
        target: machine.target.clone(),
        session: machine.session.clone(),
    }
}

fn companion_action(action: BrowserCompanionAction) -> Option<CompanionAction> {
    Some(match action {
        BrowserCompanionAction::Start => CompanionAction::Start,
        BrowserCompanionAction::Stop => CompanionAction::Stop,
        BrowserCompanionAction::Restart => CompanionAction::Restart,
        BrowserCompanionAction::Focus => CompanionAction::Focus,
        _ => return None,
    })
}

fn linked(news: HelperNews) -> (Option<LinkedBrowser>, Option<String>) {
    let browser = news
        .view
        .filter(|_| news.connected)
        .map(|view| LinkedBrowser {
            link: view.link,
            owner: view.owner,
            text: view.text,
            companion: view.companion,
            reports: view.reports,
        });
    (browser, news.notice)
}

impl ShellLinks {
    /// `enabled`: `[browser] remote_machines`, this platform starts
    /// helpers, and this client is not `herdr --remote`.
    pub(crate) fn new(enabled: bool) -> Self {
        Self::with_starter(
            enabled,
            Box::new(|machine, cycle_keys| {
                LinkClient::start(LinkClientConfig::for_machine(machine, cycle_keys)?)
            }),
        )
    }

    pub(crate) fn with_starter(enabled: bool, start: Starter) -> Self {
        Self {
            enabled,
            start,
            running: HashMap::new(),
            advertised: HashSet::new(),
            failed: HashSet::new(),
        }
    }

    /// Every loop pass: what the shell asks of the helpers, in its order.
    pub(crate) fn deliver(&self, shell: &mut ClientShellState) {
        for intent in shell.take_browser_link_intents() {
            let endpoint = match &intent {
                LinkIntent::Placement { endpoint, .. } | LinkIntent::Companion { endpoint, .. } => {
                    endpoint
                }
            };
            let ClientEndpointId::Ssh(profile) = endpoint else {
                continue;
            };
            let Some(running) = self.running.get(profile) else {
                continue;
            };
            match intent {
                LinkIntent::Placement { placement, .. } => running.client.set_placement(
                    placement.spec,
                    placement.active,
                    placement.focused,
                ),
                LinkIntent::Companion { action, .. } => {
                    if let Some(action) = companion_action(action) {
                        running.client.companion(action);
                    }
                }
            }
        }
    }

    /// On the client's timer: the helpers' clients follow the catalog and
    /// the servers' capabilities, and what the helpers said goes into the
    /// shell (its asks are taken like the server's relays).
    pub(crate) fn poll(
        &mut self,
        shell: &mut ClientShellState,
        machines: &[SavedSshEndpoint],
        endpoints: &EndpointRegistry,
        outcome: &mut ClientShellInput,
    ) {
        let mut wanted = HashMap::new();
        if self.enabled {
            for machine in machines.iter().filter(|machine| machine.enabled) {
                let endpoint = ClientEndpointId::Ssh(machine.id.clone());
                if let Some(connection) = endpoints.connection(&endpoint) {
                    if connection.negotiation.supports_capability(LINK_CAPABILITY) {
                        self.advertised.insert(machine.id.clone());
                    } else {
                        self.advertised.remove(&machine.id);
                    }
                }
                if self.advertised.contains(&machine.id) {
                    wanted.insert(machine.id.clone(), facts(machine));
                }
            }
        }
        let gone: Vec<ProfileId> = self
            .running
            .iter()
            .filter(|(id, running)| wanted.get(*id) != Some(&running.facts))
            .map(|(id, _)| id.clone())
            .collect();
        for id in gone {
            self.running.remove(&id);
            shell.receive_browser_link(
                &ClientEndpointId::Ssh(id),
                None,
                None,
                Vec::new(),
                Vec::new(),
                outcome,
            );
        }
        self.failed.retain(|id| wanted.contains_key(id));
        for (id, machine) in wanted {
            if self.running.contains_key(&id) || self.failed.contains(&id) {
                continue;
            }
            match (self.start)(machine.clone(), shell.browser_link_cycle_keys()) {
                Ok(client) => {
                    self.running.insert(
                        id,
                        Running {
                            facts: machine,
                            client,
                        },
                    );
                }
                Err(err) => {
                    tracing::warn!(machine = %machine.label, %err, "the machine's browser link did not start");
                    self.failed.insert(id);
                }
            }
        }
        for (id, running) in &self.running {
            let (actions, cycles) = running.client.take_asks();
            let (browser, notice) = linked(running.client.news());
            shell.receive_browser_link(
                &ClientEndpointId::Ssh(id.clone()),
                browser,
                notice,
                actions,
                cycles,
                outcome,
            );
        }
    }
}

#[cfg(test)]
mod tests;
