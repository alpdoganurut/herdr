use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::super::api_socket::tests::{short_dir, ShortDir};
use super::super::clients::{ClientHandler, Clients, HelperMessage, LinkView};
use super::*;
use crate::api::schema::{BrowserPlacementKind, BrowserPlacementSpec};
use crate::client::endpoint::{EndpointNegotiation, EndpointTransport};
use crate::protocol::ClientMessage as ServerMessage;

const PROFILE: &str = "0123456789abcdef0123456789abcdef";

#[derive(Default)]
struct Seen {
    hellos: Mutex<Vec<u64>>,
    placements: Mutex<Vec<(Option<String>, bool)>>,
    companions: Mutex<Vec<CompanionAction>>,
}

impl ClientHandler for Seen {
    fn hello(&self, client: u64, _: &MachineFacts, _: Vec<String>) {
        self.hellos.lock().unwrap().push(client);
    }
    fn placement(&self, _: u64, spec: Option<BrowserPlacementSpec>, active: bool, _: bool) {
        self.placements
            .lock()
            .unwrap()
            .push((spec.map(|spec| spec.id), active));
    }
    fn companion(&self, _: u64, action: CompanionAction) {
        self.companions.lock().unwrap().push(action);
    }
    fn gone(&self, _: u64) {}
}

struct Quiet;

impl EndpointTransport for Quiet {
    fn send(&mut self, _: &ServerMessage) -> io::Result<()> {
        Ok(())
    }
}

fn profile() -> ProfileId {
    ProfileId::parse(PROFILE).unwrap()
}

fn endpoint() -> ClientEndpointId {
    ClientEndpointId::Ssh(profile())
}

fn machine() -> SavedSshEndpoint {
    SavedSshEndpoint {
        id: profile(),
        label: "remote".into(),
        target: "remote.invalid".into(),
        session: "default".into(),
        enabled: true,
    }
}

fn registry(capabilities: &[&str]) -> EndpointRegistry {
    let mut registry = EndpointRegistry::empty();
    registry.insert(
        endpoint(),
        Quiet,
        1,
        EndpointNegotiation::new(
            Vec::new(),
            capabilities.iter().map(|c| (*c).to_owned()).collect(),
        ),
        true,
    );
    registry
}

fn shell() -> ClientShellState {
    ClientShellState::new(crate::client::shell::ClientShellConfig::from_config(
        &crate::config::Config::default(),
    ))
}

fn wait_for(mut what: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !what() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A fake helper on a socket of its own, and links whose start reaches it
/// (never `herdr browser-link`).
struct Rig {
    _dir: ShortDir,
    seen: Arc<Seen>,
    helper: Arc<Mutex<Option<Clients>>>,
    starts: Arc<Mutex<u32>>,
    links: ShellLinks,
}

fn rig(enabled: bool) -> Rig {
    let dir = short_dir();
    let socket: PathBuf = dir.path().join("k.link");
    let seen = Arc::new(Seen::default());
    let helper: Arc<Mutex<Option<Clients>>> = Arc::default();
    let starts = Arc::new(Mutex::new(0));
    let start: Starter = {
        let (seen, helper, starts) = (Arc::clone(&seen), Arc::clone(&helper), Arc::clone(&starts));
        Box::new(move |facts, keys| {
            *starts.lock().unwrap() += 1;
            let (seen, helper, bound) = (Arc::clone(&seen), Arc::clone(&helper), socket.clone());
            let spawn = Arc::new(move || {
                let clients = Clients::bind(&bound, Arc::clone(&seen) as Arc<dyn ClientHandler>)?;
                *helper.lock().unwrap() = Some(clients);
                Ok(())
            });
            let mut config = LinkClientConfig::new(facts, keys, socket.clone(), spawn);
            config.retry = Duration::from_millis(20);
            LinkClient::start(config)
        })
    };
    Rig {
        _dir: dir,
        seen,
        helper,
        starts,
        links: ShellLinks::with_starter(enabled, start),
    }
}

fn say(rig: &Rig, link: &str, state: &str) {
    let view = LinkView {
        link: link.into(),
        companion: crate::api::schema::BrowserCompanionAppInfo {
            state: state.into(),
            ..Default::default()
        },
        ..Default::default()
    };
    if let Some(clients) = rig.helper.lock().unwrap().as_ref() {
        clients.broadcast(&HelperMessage::State(Box::new(view)));
    }
}

fn poll(
    rig: &mut Rig,
    shell: &mut ClientShellState,
    registry: &EndpointRegistry,
) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    rig.links.poll(shell, &[machine()], registry, &mut outcome);
    outcome
}

/// A machine whose server advertises `browser-link.v1` gets a helper's
/// client; what the helper says reaches the shell (its companion becomes
/// the dock's), and the shell's asks reach the helper.
#[test]
fn an_advertising_machine_gets_a_helper_and_the_shell_follows_it() {
    let mut rig = rig(true);
    let mut shell = shell();
    shell.show_endpoint_for_test(endpoint());
    let registry = registry(&[LINK_CAPABILITY]);
    poll(&mut rig, &mut shell, &registry);
    assert_eq!(*rig.starts.lock().unwrap(), 1);
    wait_for(|| !rig.seen.hellos.lock().unwrap().is_empty());
    assert!(!shell.browser_dock_available(), "nothing said yet");

    say(&rig, "linked", "stopped");
    wait_for(|| {
        poll(&mut rig, &mut shell, &registry);
        shell.browser_dock_available()
    });
    assert_eq!(
        shell.browser_companion().map(|c| c.state.as_str()),
        Some("stopped"),
        "the helper's companion"
    );
    // the user's start goes to the helper
    let mut outcome = ClientShellInput::default();
    shell.request_browser_companion(BrowserCompanionAction::Start, &mut outcome);
    assert!(
        outcome.requests.is_empty(),
        "nothing goes to the machine's server"
    );
    rig.links.deliver(&mut shell);
    wait_for(|| *rig.seen.companions.lock().unwrap() == [CompanionAction::Start]);

    // the same machine again: no second client
    poll(&mut rig, &mut shell, &registry);
    assert_eq!(*rig.starts.lock().unwrap(), 1);
}

/// Without the capability, with the feature off, or once the machine is
/// gone from the catalog, there is no helper and no dock.
#[test]
fn no_capability_no_feature_or_no_machine_means_no_helper() {
    let mut rig = rig(true);
    let mut shell = shell();
    poll(&mut rig, &mut shell, &registry(&[]));
    assert_eq!(
        *rig.starts.lock().unwrap(),
        0,
        "the server does not advertise it"
    );

    let mut off = self::rig(false);
    poll(&mut off, &mut shell, &registry(&[LINK_CAPABILITY]));
    assert_eq!(*off.starts.lock().unwrap(), 0, "remote_machines is off");

    let registry = registry(&[LINK_CAPABILITY]);
    shell.show_endpoint_for_test(endpoint());
    poll(&mut rig, &mut shell, &registry);
    say_when_bound(&rig);
    wait_for(|| {
        poll(&mut rig, &mut shell, &registry);
        shell.browser_dock_available()
    });
    let mut outcome = ClientShellInput::default();
    rig.links.poll(&mut shell, &[], &registry, &mut outcome);
    assert!(!shell.browser_dock_available(), "the machine was removed");
    assert!(outcome.repaint);
    assert!(rig.links.running.is_empty());
}

fn say_when_bound(rig: &Rig) {
    wait_for(|| !rig.seen.hellos.lock().unwrap().is_empty());
    say(rig, "linked", "running");
}

/// A placement of the shown machine goes to its helper, never the
/// machine's server; leaving the machine hides it there first.
#[test]
fn placements_go_to_the_helper_and_a_switch_hides_the_machine_left() {
    let mut rig = rig(true);
    let mut shell = shell();
    shell.show_endpoint_for_test(endpoint());
    let registry = registry(&[LINK_CAPABILITY]);
    poll(&mut rig, &mut shell, &registry);
    say_when_bound(&rig);
    wait_for(|| {
        poll(&mut rig, &mut shell, &registry);
        shell.browser_dock_available()
    });
    let spec = BrowserPlacementSpec {
        id: "pl-1".into(),
        kind: BrowserPlacementKind::Side,
        tab_id: "t1".into(),
        ..Default::default()
    };
    shell.place_on_link_for_test(
        &endpoint(),
        crate::client::shell::LinkPlacement {
            spec: Some(spec),
            active: true,
            focused: true,
        },
    );
    rig.links.deliver(&mut shell);
    shell.show_endpoint_for_test(ClientEndpointId::Local);
    rig.links.deliver(&mut shell);
    wait_for(|| rig.seen.placements.lock().unwrap().len() == 2);
    assert_eq!(
        *rig.seen.placements.lock().unwrap(),
        [(Some("pl-1".into()), true), (Some("pl-1".into()), false)]
    );
}
