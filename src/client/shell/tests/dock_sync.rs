//! Fork (dock sync): a deterministic stress test of the browser dock under
//! fast tab and machine switching (`.local/prd/dock-sync/map.md`, F6).
//!
//! The client is the real one: `ClientState`, the endpoint registry, the
//! activation transaction and the loop's arms (`loop_arms.rs`,
//! `shell_runtime.rs`). Each endpoint is a fake server: it consumes what the
//! client sent, in order, and answers through a queue; a seeded random
//! number generator decides when either side's queue moves, what the user
//! does next and how far the virtual clock advances. Each fake keeps a real
//! placement board (`PlacementBoard::new()`), and the board is what the
//! companion shows (it follows the board's latest list, level-triggered).
//!
//! What it checks is what was drawn: every frame that reached the (sink)
//! terminal records its band, Browser view area, pane rect, surface size and
//! tab (`DrawnDock`). After every settle (all queues drained, the dock's
//! gap and a tick passed):
//! - I1: the board shows the placement exactly when the drawn frame has a
//!   band (or a Browser view) and no modal is open;
//! - I2: the shown area is the drawn band right of its divider (or the
//!   drawn Browser view's area);
//! - I3: the shown tab's PTY size equals the drawn pane rect and the drawn
//!   surface frame;
//! - I4: the board's tab, the server's focused tab and the drawn tab agree.
//!
//! At every step where the server has read all the client sent and a frame
//! with panes is on screen, the board's shown placement is checked against
//! that frame:
//! - I5a (window over text): the frame has no band (or Browser view) for
//!   it, shows another tab, or the window's area overlaps the pane rect;
//!   asserted 0 like the settled checks;
//! - I5b (text cut under the band): the band is drawn for the tab but the
//!   surface frame is still wider (or taller) than the pane, its PTY not yet
//!   resized. Counted, not asserted: a size report carries no tab identity
//!   (map R6), so a report meant for the tab left can land on the tab shown
//!   for one round trip; a divider drag or a terminal resize shows the same
//!   flash by design (the window stays, it does not blink).
//!
//! I5a + I5b is the map's I5. A frame without panes (no surface yet, or
//! after a terminal resize until the next surface) counts in neither: the
//! terminal shows no pane text then. The companion's own hiding while the
//! terminal window moves is not modelled.
//!
//! The harness reimplements, as glue: the loop's dispatch of the three
//! server messages it fakes (pane surface, response chunk, endpoint
//! control); the Timer arm's shell part reduced to a failed transport
//! (`handle_endpoint_disconnect`, then the supervisor's reconnect to the
//! same server boot), `tick_browser` and `tick_browser_dock`; the
//! `ActivateEndpoint` arm's catalog selection. It never runs health checks,
//! command timeouts or activation timeouts (the fakes answer everything,
//! and the virtual clock never reaches an activation's real deadline).
//!
//! The fakes assume what the server does today: a focus change or
//! `surface.set(true)` sizes the shown tab with the client's last reported
//! size, and a size report resizes whichever tab is shown; a render sends a
//! snapshot when the focus changed (or after `surface.set(true)`, which
//! moves the revision twice) and a full surface at the shown tab's size,
//! never a patch; answers leave before the render's messages; one client
//! per server.

use super::*;
use crate::api::schema::{
    BrowserCompanionAppInfo, BrowserGetInfo, BrowserPlacementKind, BrowserSessionInfo, ErrorBody,
    ErrorResponse, Method, Request, ResponseResult, SuccessResponse, TabInfo,
};
use crate::browser::placements::{ClientFacts, PlacementBoard};
use crate::client::endpoint::{
    self, ClientEndpointId, EndpointNegotiation, EndpointRegistry, PendingEndpointActivation,
    ProfileId, SavedSshEndpoint,
};
use crate::client::endpoint_commands::EndpointCommands;
use crate::client::events::ClientLoopEvent;
use crate::client::state::ClientState;
use crate::config::Config;
use crate::protocol::{ClientMessage, ClientShellSnapshot, PaneSurfaceFrame, ServerMessage};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const COLS: u16 = 160;
const ROWS: u16 = 45;
const TABS: [&str; 3] = ["tab_a", "tab_b", "tab_c"];
/// The machine's tabs (it has no dock: no browser helper is attached).
const MACHINE_TABS: [&str; 2] = ["tab_m1", "tab_m2"];

/// xorshift64*: a fixed seed replays the same run.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

type Inbox = Arc<Mutex<Vec<ClientMessage>>>;

/// The client's side of a fake connection: what it sends waits in the
/// fake's inbox until the fake reads it.
#[derive(Clone)]
struct Capture(Inbox);

impl endpoint::EndpointTransport for Capture {
    fn send(&mut self, message: &ClientMessage) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(message.clone());
        Ok(())
    }
}

fn negotiation() -> EndpointNegotiation {
    EndpointNegotiation::new(
        vec!["client_shell.surface.set".into()],
        vec![
            crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY.into(),
            crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY.into(),
        ],
    )
}

/// One fake server (one endpoint, one client connection).
struct FakeServer {
    endpoint: ClientEndpointId,
    generation: u64,
    boot: String,
    client_id: u64,
    inbox: Inbox,
    outbox: VecDeque<ServerMessage>,
    rev: u64,
    tabs: Vec<String>,
    focused: String,
    /// Each tab's PTY size (the size its surface frame is drawn at).
    pty: HashMap<String, (u16, u16)>,
    /// The client's last reported surface size (`ClientShellResize`).
    terminal: Option<(u16, u16)>,
    surface_active: bool,
    /// The focused tab the last snapshot carried (the snapshot's content).
    sent_focus: Option<String>,
    force_snapshot: bool,
    board: PlacementBoard,
    pages: HashMap<String, u32>,
    /// Every size the client reported, in order.
    resizes: Vec<(u16, u16)>,
}

impl FakeServer {
    fn new(endpoint: ClientEndpointId, generation: u64, boot: &str, tabs: &[&str]) -> Self {
        Self {
            endpoint,
            generation,
            boot: boot.into(),
            client_id: 1,
            inbox: Arc::new(Mutex::new(Vec::new())),
            outbox: VecDeque::new(),
            rev: 0,
            tabs: tabs.iter().map(|tab| (*tab).to_owned()).collect(),
            focused: tabs[0].to_owned(),
            pty: tabs
                .iter()
                .map(|tab| ((*tab).to_owned(), (120, 40)))
                .collect(),
            terminal: None,
            surface_active: false,
            sent_focus: None,
            force_snapshot: true,
            board: PlacementBoard::new(),
            pages: HashMap::new(),
            resizes: Vec::new(),
        }
    }

    fn inbox_len(&self) -> usize {
        self.inbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    fn facts(&self) -> ClientFacts {
        ClientFacts {
            focused: true,
            foreground: true,
            surface_active: self.surface_active,
        }
    }

    fn snapshot(&self) -> ClientShellSnapshot {
        let mut snapshot = snapshot();
        snapshot.boot_id = self.boot.clone();
        snapshot.revision = self.rev;
        snapshot.tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab_id)| ClientShellTab {
                tab_id: tab_id.clone(),
                workspace_id: "ws_1".into(),
                number: index + 1,
                label: tab_id.clone(),
                custom_label: true,
                zoomed: false,
                focused: *tab_id == self.focused,
                agent_status: AgentStatus::Idle,
                color: None,
                important: false,
                remind_every: None,
            })
            .collect();
        snapshot.focused_tab_id = Some(self.focused.clone());
        snapshot.workspaces[0].active_tab_id = self.focused.clone();
        let pane_id = format!("pane_{}", self.focused);
        snapshot.focused_pane_id = Some(pane_id.clone());
        snapshot.panes[0].pane_id = pane_id;
        snapshot.panes[0].tab_id = self.focused.clone();
        snapshot
    }

    fn surface(&self) -> PaneSurfaceFrame {
        let (cols, rows) = self.pty[&self.focused];
        let buffer = Buffer::empty(Rect::new(0, 0, cols, rows));
        let rect = crate::protocol::SurfaceRect {
            x: 0,
            y: 0,
            width: cols,
            height: rows,
        };
        PaneSurfaceFrame {
            boot_id: self.boot.clone(),
            projection_revision: self.rev,
            surface_revision: 0,
            frame: FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, None, &[]),
            panes: vec![PaneSurfacePane {
                pane_id: format!("pane_{}", self.focused),
                content_revision: 0,
                rect,
                inner_rect: rect,
                scrollbar_rect: None,
                scroll: None,
                focused: true,
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                alternate_screen_active: false,
                pixel_width: 0,
                pixel_height: 0,
            }],
            splits: Vec::new(),
            popup: None,
            graphics: crate::protocol::SurfaceGraphicsScene::default(),
        }
    }

    /// The server's render pass: a changed (or forced) snapshot gets the
    /// next revision; an active surface gets a frame at the shown tab's size.
    fn render(&mut self) {
        if self.force_snapshot || self.sent_focus.as_deref() != Some(self.focused.as_str()) {
            self.force_snapshot = false;
            self.rev += 1;
            self.sent_focus = Some(self.focused.clone());
            self.outbox.push_back(
                crate::protocol::endpoint::snapshot_message(&self.snapshot())
                    .unwrap_or_else(|error| panic!("snapshot encodes: {error}")),
            );
        }
        if self.surface_active {
            self.outbox
                .push_back(ServerMessage::PaneSurface(self.surface()));
        }
    }

    /// The shown tab takes the client's last reported size (the server's
    /// geometry claim on focus and on a resize).
    fn size_shown_tab(&mut self) {
        if let Some(size) = self.terminal {
            self.pty.insert(self.focused.clone(), size);
        }
    }

    fn respond(&mut self, boot_id: &str, id: &str, result: Result<ResponseResult, &str>) {
        let data = match result {
            Ok(result) => serde_json::to_vec(&SuccessResponse {
                id: id.into(),
                result,
            }),
            Err(code) => serde_json::to_vec(&ErrorResponse {
                id: id.into(),
                error: ErrorBody {
                    code: code.into(),
                    message: code.into(),
                },
            }),
        }
        .unwrap_or_default();
        self.outbox
            .push_back(ServerMessage::ClientShellEndpointResponseChunk {
                boot_id: boot_id.into(),
                request_id: id.into(),
                final_chunk: true,
                data,
            });
    }

    fn browser_info(&self) -> BrowserGetInfo {
        BrowserGetInfo {
            seq: 1,
            enabled: true,
            companion_app: Some(BrowserCompanionAppInfo {
                state: "running".into(),
                version: Some("0.2.4".into()),
                ..Default::default()
            }),
            window_off: true,
            sessions: self
                .pages
                .iter()
                .filter(|(_, pages)| **pages > 0)
                .map(|(tab_id, pages)| BrowserSessionInfo {
                    tab_id: tab_id.clone(),
                    pages: *pages,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    /// Read the next message the client sent; `false` when none waits.
    fn process_one(&mut self) -> bool {
        let message = {
            let mut inbox = self
                .inbox
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if inbox.is_empty() {
                return false;
            }
            inbox.remove(0)
        };
        if trace() {
            eprintln!(
                "  server {:?} reads {}",
                self.endpoint.is_local(),
                brief_client(&message)
            );
        }
        match message {
            ClientMessage::ClientShellResize { surface_size, .. } => {
                self.terminal = Some((surface_size.cols, surface_size.rows));
                self.resizes.push((surface_size.cols, surface_size.rows));
                self.size_shown_tab();
                self.render();
            }
            ClientMessage::EndpointControl { kind, data }
                if kind == crate::protocol::endpoint::PRESENTATION_EFFECTS_SYNC_KIND =>
            {
                self.outbox.push_back(ServerMessage::EndpointControl {
                    kind: crate::protocol::endpoint::PRESENTATION_EFFECTS_READY_KIND.into(),
                    data,
                });
            }
            ClientMessage::ClientShellEndpointRequest { boot_id, request } => {
                let Ok(request) = serde_json::from_str::<Request>(&request) else {
                    return true;
                };
                if boot_id != self.boot {
                    self.respond(&boot_id, &request.id, Err("stale_boot"));
                    return true;
                }
                self.request(&boot_id, request);
            }
            _ => {}
        }
        true
    }

    fn request(&mut self, boot_id: &str, request: Request) {
        match request.method {
            Method::ClientShellSurfaceSet(params) => {
                if params.active {
                    self.rev += 1;
                    self.force_snapshot = true;
                }
                self.surface_active = params.active;
                if params.active {
                    self.size_shown_tab();
                }
                self.board.set_facts(&[(self.client_id, self.facts())]);
                self.respond(
                    boot_id,
                    &request.id,
                    Ok(ResponseResult::ClientShellSurfaceSet {
                        active: params.active,
                        projection_revision: self.rev,
                    }),
                );
                if params.active {
                    self.render();
                }
            }
            Method::TabFocus(target) => {
                if !self.surface_active {
                    self.respond(boot_id, &request.id, Err("surface_inactive"));
                    return;
                }
                if self.tabs.contains(&target.tab_id) {
                    self.focused = target.tab_id.clone();
                    self.size_shown_tab();
                }
                let tab = TabInfo {
                    tab_id: target.tab_id,
                    workspace_id: "ws_1".into(),
                    number: 1,
                    label: String::new(),
                    focused: true,
                    pane_count: 1,
                    agent_status: AgentStatus::Idle,
                    color: None,
                    important: false,
                    remind_every: None,
                    pinned: false,
                    muted: false,
                    dnd: false,
                };
                self.respond(boot_id, &request.id, Ok(ResponseResult::TabInfo { tab }));
                self.render();
            }
            Method::BrowserPlacementSet(params) => {
                self.board
                    .set(self.client_id, params.placement, self.facts(), 0);
                self.respond(boot_id, &request.id, Ok(ResponseResult::Ok {}));
            }
            Method::BrowserGet(_) => {
                let browser = self.browser_info();
                self.respond(
                    boot_id,
                    &request.id,
                    Ok(ResponseResult::BrowserGet { browser }),
                );
            }
            _ => self.respond(boot_id, &request.id, Ok(ResponseResult::Ok {})),
        }
    }

    /// The board's entry for this client, as the companion reads it.
    fn shown_placement(&self) -> Option<crate::api::schema::BrowserPlacementSpec> {
        let running = AtomicBool::new(true);
        self.board
            .wait(None, Duration::ZERO, &running)
            .placements
            .into_iter()
            .map(|info| info.placement)
            .find(|placement| placement.shown)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    /// Local: A docked, B not; switches only.
    LocalSwitch,
    /// Local: and C in Browser view.
    LocalBrowserView,
    /// Local (dock) and a saved machine (none), through activations.
    MachineRoundTrip,
    /// Local tab clicks while a machine activation is pending.
    LocalClickDuringActivation,
    /// A same-boot reconnect of Local mid-run.
    Reconnect,
    /// `browser.get` pages toggling (the strip and the band's title row).
    PagesToggle,
    /// Terminal resizes mid-run.
    TerminalResize,
    /// A modal opened and closed over the band.
    Modal,
}

const SCENARIOS: [Scenario; 8] = [
    Scenario::LocalSwitch,
    Scenario::LocalBrowserView,
    Scenario::MachineRoundTrip,
    Scenario::LocalClickDuringActivation,
    Scenario::Reconnect,
    Scenario::PagesToggle,
    Scenario::TerminalResize,
    Scenario::Modal,
];

#[derive(Default, Debug, Clone)]
struct Failures {
    /// No pane frame drawn after a settle, or an activation still pending.
    unsettled: u32,
    i1: u32,
    i2: u32,
    i3: u32,
    i4: u32,
    i5a: u32,
    i5b: u32,
    first: Vec<String>,
}

impl Failures {
    fn total_settled(&self) -> u32 {
        self.unsettled + self.i1 + self.i2 + self.i3 + self.i4
    }

    fn note(&mut self, what: String) {
        if self.first.len() < 3 {
            self.first.push(what);
        }
    }
}

struct Harness {
    rng: Rng,
    scenario: Scenario,
    now: Instant,
    tick_at: Option<Instant>,
    state: ClientState,
    endpoints: EndpointRegistry,
    pending: Option<PendingEndpointActivation>,
    commands: EndpointCommands,
    prefix: crate::platform::RealPrefixInputSource,
    scheduled: Option<ClientLoopEvent>,
    serial: u64,
    local: FakeServer,
    machine: FakeServer,
    machine_id: ClientEndpointId,
    catalog: endpoint::EndpointCatalog,
    supervisors: endpoint::EndpointSupervisors,
    failures: Failures,
}

impl Harness {
    /// `spaces`: the spaces sidebar layout (its view strip), else tabs (its
    /// session header and title row).
    fn new(seed: u64, scenario: Scenario, spaces: bool) -> Self {
        Self::with_local_tabs(seed, scenario, spaces, &TABS)
    }

    fn with_local_tabs(seed: u64, scenario: Scenario, spaces: bool, local_tabs: &[&str]) -> Self {
        let mut config = Config::default();
        if spaces {
            config.ui.sidebar_layout = crate::config::SidebarLayoutConfig::Spaces;
        }
        let mut state = ClientState::test_new();
        state.reported_size = (COLS, ROWS);
        state.reported_cell_size = (10, 20);
        state.shell = Some(ClientShellState::new(ClientShellConfig::from_config(
            &config,
        )));
        let profile = SavedSshEndpoint {
            id: ProfileId::parse("0123456789abcdef0123456789abcdef").expect("profile id"),
            label: "devhost1".into(),
            target: "dev@devhost1.example".into(),
            session: "main".into(),
            enabled: true,
        };
        let machine_id = ClientEndpointId::Ssh(profile.id.clone());
        let machine_tabs: &[&str] = &MACHINE_TABS;
        let local = FakeServer::new(ClientEndpointId::Local, 1, "local-boot", local_tabs);
        let machine = FakeServer::new(machine_id.clone(), 7, "devhost1-boot", machine_tabs);
        let mut endpoints = EndpointRegistry::new(Capture(local.inbox.clone()), 1, negotiation());
        endpoints.insert(
            machine_id.clone(),
            Capture(machine.inbox.clone()),
            7,
            negotiation(),
            false,
        );
        {
            let shell = state.shell.as_mut().expect("shell");
            shell.set_endpoint_catalog(std::slice::from_ref(&profile));
            shell.browser_dock.extent_for_test = Some((COLS, ROWS, 1600, 900));
        }
        let mut catalog = endpoint::EndpointCatalog::default();
        catalog.ssh.push(profile.clone());
        let mut harness = Self {
            rng: Rng::new(seed),
            scenario,
            now: Instant::now(),
            tick_at: None,
            state,
            endpoints,
            pending: None,
            commands: EndpointCommands::default(),
            prefix: crate::platform::RealPrefixInputSource::default(),
            scheduled: None,
            serial: 1,
            local,
            machine,
            machine_id,
            catalog,
            supervisors: endpoint::EndpointSupervisors::new(&[], Instant::now()),
            failures: Failures::default(),
        };
        harness.local.surface_active = true;
        harness.local.terminal = Some((110, 40));
        harness.local.render();
        harness.machine.render();
        harness.settle();
        // the browser is known (as after the first `browser.get`), tab A docks
        let browser = harness.local.browser_info();
        harness.shell().browser.info = Some(browser);
        harness.toggle_dock();
        if scenario == Scenario::LocalBrowserView {
            harness.click_local_tab("tab_c");
            harness.settle();
            harness.toggle_view();
            harness.click_local_tab("tab_a");
        }
        harness.settle();
        harness
    }

    fn shell(&mut self) -> &mut ClientShellState {
        self.state.shell.as_mut().expect("shell")
    }

    // ------------------------------------------------------------ the client

    fn finish(
        &mut self,
        outcome: ClientShellInput,
        frame: Option<crate::client::frame_output::ComposedFrame>,
    ) {
        crate::client::shell_runtime::finish_client_shell_input(
            &mut self.state,
            outcome,
            frame,
            &mut self.endpoints,
            &mut self.pending,
            &mut self.commands,
            &mut self.prefix,
            &mut self.scheduled,
            self.now,
        )
        .expect("finish input");
        self.after_event();
    }

    fn compose(&mut self) -> Option<crate::client::frame_output::ComposedFrame> {
        let (cols, rows) = self.state.reported_size;
        self.shell().compose(cols, rows)
    }

    fn toggle_dock(&mut self) {
        let mut outcome = ClientShellInput::default();
        self.shell()
            .toggle_browser_dock_for_focused_tab(&mut outcome);
        let frame = self.compose();
        self.finish(outcome, frame);
    }

    fn toggle_view(&mut self) {
        let mut outcome = ClientShellInput::default();
        self.shell()
            .toggle_browser_view_for_focused_tab(&mut outcome);
        let frame = self.compose();
        self.finish(outcome, frame);
    }

    /// A tab click on Local (with machines configured, the tabs layout's
    /// click goes through an activation, which a pending one retargets or
    /// is replaced by).
    fn click_local_tab(&mut self, tab: &str) {
        self.activate(
            ClientEndpointId::Local,
            Some(ClientEndpointFocusTarget::Tab(tab.into())),
            false,
        );
    }

    fn activate(
        &mut self,
        endpoint_id: ClientEndpointId,
        target: Option<ClientEndpointFocusTarget>,
        force: bool,
    ) {
        if trace() {
            eprintln!(
                "USER activate local={} {target:?} pending={}",
                endpoint_id.is_local(),
                self.pending.is_some()
            );
        }
        self.begin(endpoint_id, target, force);
        self.after_event();
    }

    /// The loop's `ClientLoopEvent::ActivateEndpoint` arm.
    fn begin(
        &mut self,
        endpoint_id: ClientEndpointId,
        target: Option<ClientEndpointFocusTarget>,
        force: bool,
    ) {
        if !self.catalog.select_endpoint(&endpoint_id) {
            return;
        }
        crate::client::shell_runtime::begin_endpoint_activation(
            &mut self.state,
            &mut self.endpoints,
            &mut self.commands,
            &mut self.pending,
            &mut self.serial,
            endpoint_id,
            target,
            force,
            self.now,
            &mut self.scheduled,
        )
        .expect("activation begins");
    }

    /// What the loop does between events: an immediate event first, then
    /// its checks before it waits (`reconcile_after_event`).
    fn after_event(&mut self) {
        for _ in 0..8 {
            let Some(event) = self.scheduled.take() else {
                break;
            };
            if let ClientLoopEvent::ActivateEndpoint {
                endpoint_id,
                target,
                force,
            } = event
            {
                self.begin(endpoint_id, target, force);
            }
        }
        let now = self.now;
        crate::client::loop_arms::reconcile_after_event(
            &mut self.state,
            &mut self.endpoints,
            &mut self.pending,
            now,
        )
        .expect("reconcile");
        let delay = self
            .state
            .shell
            .as_ref()
            .map_or(Duration::from_millis(100), |shell| shell.timer_delay(now));
        let requested = now + delay;
        self.tick_at = Some(self.tick_at.map_or(requested, |at| at.min(requested)));
    }

    /// The loop's handling of one server message from `endpoint_id`.
    fn deliver(&mut self, endpoint_id: ClientEndpointId, generation: u64, message: ServerMessage) {
        if !self.endpoints.accepts(&endpoint_id, generation) {
            return;
        }
        let Some(route) = crate::client::loop_arms::route_server_message(
            &self.state,
            &self.endpoints,
            &self.pending,
            &self.commands,
            &endpoint_id,
            generation,
            &message,
        ) else {
            return;
        };
        let event = match message {
            ServerMessage::PaneSurface(surface) => crate::client::loop_arms::receive_pane_surface(
                &mut self.state,
                &mut self.endpoints,
                &mut self.pending,
                &mut self.commands,
                &mut self.prefix,
                &endpoint_id,
                generation,
                surface,
                &route,
            )
            .expect("surface"),
            ServerMessage::ClientShellEndpointResponseChunk {
                boot_id,
                request_id,
                final_chunk,
                data,
            } => {
                match crate::client::loop_arms::receive_lifecycle_response(
                    &mut self.state,
                    &mut self.endpoints,
                    &mut self.pending,
                    &mut self.commands,
                    &endpoint_id,
                    generation,
                    &boot_id,
                    &request_id,
                    final_chunk,
                    &data,
                    self.now,
                )
                .expect("response")
                {
                    crate::client::loop_arms::ResponseChunk::Handled(event) => event,
                    crate::client::loop_arms::ResponseChunk::Command => {
                        crate::client::loop_arms::receive_command_response(
                            &mut self.state,
                            &mut self.endpoints,
                            &mut self.pending,
                            &mut self.commands,
                            &mut self.prefix,
                            &mut self.scheduled,
                            &endpoint_id,
                            generation,
                            &boot_id,
                            &request_id,
                            final_chunk,
                            data,
                            self.now,
                        )
                        .expect("command response");
                        None
                    }
                }
            }
            ServerMessage::EndpointControl { kind, data }
                if kind == crate::protocol::endpoint::PRESENTATION_EFFECTS_READY_KIND =>
            {
                crate::client::loop_arms::receive_presentation_effects_ready(
                    &mut self.state,
                    &mut self.endpoints,
                    &mut self.pending,
                    &mut self.commands,
                    &endpoint_id,
                    generation,
                    &data,
                )
                .expect("effects ready")
            }
            ServerMessage::EndpointControl { kind, data } => {
                match endpoint::decode_endpoint_control(&kind, &data) {
                    Ok(endpoint::EndpointControlMessage::Snapshot(snapshot)) => {
                        let event = crate::client::loop_arms::receive_shell_snapshot(
                            &mut self.state,
                            &mut self.endpoints,
                            &mut self.pending,
                            &mut self.commands,
                            &mut self.prefix,
                            &endpoint_id,
                            generation,
                            snapshot,
                            &route,
                            self.now,
                        )
                        .expect("snapshot");
                        crate::client::loop_arms::activation_after_snapshot(
                            &mut self.state,
                            &self.endpoints,
                            &self.pending,
                            &self.catalog,
                            &endpoint_id,
                        )
                        .or(event)
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(event) = event {
            self.scheduled = Some(event);
        }
        self.after_event();
    }

    /// Deliver the next message `endpoint`'s server sent; `false` when none.
    fn deliver_one(&mut self, machine: bool) -> bool {
        let server = if machine {
            &mut self.machine
        } else {
            &mut self.local
        };
        let Some(message) = server.outbox.pop_front() else {
            return false;
        };
        if trace() {
            eprintln!(
                "  client gets from local={} {}",
                server.endpoint.is_local(),
                brief_server(&message)
            );
        }
        let (endpoint_id, generation) = (server.endpoint.clone(), server.generation);
        self.deliver(endpoint_id, generation, message);
        true
    }

    /// The Timer arm's shell part, for the dock: the browser poll and the
    /// dock's tick, then the outcome goes the way of an input's.
    fn tick(&mut self) {
        self.tick_at = None;
        let now = self.now;
        // the Timer arm: a failed transport first (its supervisor connects
        // again: a new connection to the same server boot)
        for failure in self.endpoints.take_failures() {
            if self.endpoints.connection(&failure.endpoint_id).is_some()
                && !self
                    .endpoints
                    .accepts(&failure.endpoint_id, failure.generation)
            {
                continue;
            }
            crate::client::shell_runtime::handle_endpoint_disconnect(
                &mut self.state,
                &mut self.endpoints,
                &mut self.commands,
                &mut self.supervisors,
                &mut self.pending,
                &failure.endpoint_id,
                failure.generation,
                now,
                "connection was lost; reconnecting",
            );
            self.reconnect(&failure.endpoint_id);
        }
        let mut outcome = ClientShellInput::default();
        let shell = self.shell();
        shell.tick_browser(now, &mut outcome);
        shell.tick_browser_dock(now, &mut outcome);
        let frame = if outcome.repaint {
            self.compose()
        } else {
            None
        };
        self.finish(outcome, frame);
    }

    fn advance(&mut self, by: Duration) {
        self.now += by;
        if self.tick_at.is_none_or(|at| self.now >= at) {
            self.tick();
        }
    }

    // ------------------------------------------------------------ the user

    fn user_action(&mut self) {
        let pick = self.rng.below(100);
        match self.scenario {
            Scenario::LocalSwitch | Scenario::Reconnect | Scenario::PagesToggle => {
                let tab = TABS[self.rng.below(2) as usize];
                self.click_local_tab(tab);
            }
            Scenario::LocalBrowserView | Scenario::TerminalResize | Scenario::Modal => {
                let tab = TABS[self.rng.below(3) as usize];
                self.click_local_tab(tab);
            }
            Scenario::MachineRoundTrip => {
                if self.endpoints.active_id().is_local() && pick < 50 {
                    let machine = self.machine_id.clone();
                    self.activate(machine, None, false);
                } else {
                    let tab = TABS[self.rng.below(2) as usize];
                    self.click_local_tab(tab);
                }
            }
            Scenario::LocalClickDuringActivation => {
                if pick < 40 {
                    let machine = self.machine_id.clone();
                    self.activate(machine, None, false);
                } else {
                    let tab = TABS[self.rng.below(2) as usize];
                    self.click_local_tab(tab);
                }
            }
        }
        match self.scenario {
            Scenario::Reconnect if self.rng.chance(15) => self.reconnect_local(),
            Scenario::PagesToggle if self.rng.chance(40) => {
                let tab = TABS[self.rng.below(2) as usize].to_owned();
                let pages = self.local.pages.entry(tab).or_insert(0);
                *pages = if *pages == 0 { 1 } else { 0 };
                // until the next `browser.get` poll (2 s while docked)
                self.advance(Duration::from_millis(2100));
            }
            Scenario::TerminalResize if self.rng.chance(25) => {
                let cols = [150, 160, 172][self.rng.below(3) as usize];
                let rows = [40, 45][self.rng.below(2) as usize];
                self.resize_terminal(cols, rows);
            }
            Scenario::Modal if self.rng.chance(25) => self.toggle_modal(),
            _ => {}
        }
    }

    fn resize_terminal(&mut self, cols: u16, rows: u16) {
        self.shell().browser_dock.extent_for_test =
            Some((cols, rows, u32::from(cols) * 10, u32::from(rows) * 20));
        crate::client::loop_arms::apply_terminal_resize(
            &mut self.state,
            &mut self.endpoints,
            &mut self.pending,
            cols,
            rows,
            10,
            20,
            false,
            false,
        )
        .expect("resize");
        self.after_event();
    }

    fn toggle_modal(&mut self) {
        let mut outcome = ClientShellInput::default();
        let shell = self.shell();
        if shell.overlay.is_some() {
            shell.overlay = None;
            outcome.repaint = true;
        } else {
            shell.open_browser_overlay(&mut outcome);
        }
        let frame = self.compose();
        self.finish(outcome, frame);
    }

    /// Local's connection drops (the next tick finds it) and comes back to
    /// the same server boot.
    fn reconnect_local(&mut self) {
        self.endpoints.fail(
            &ClientEndpointId::Local,
            std::io::Error::other("the connection dropped"),
        );
        self.after_event();
    }

    /// A new connection to `endpoint_id`'s server (same boot): the server
    /// forgets the old one (and its placement), the client registers the new
    /// one surface-inactive and gets its first snapshot (the loop's
    /// supervisor `Connected` arm).
    fn reconnect(&mut self, endpoint_id: &ClientEndpointId) {
        let server = if endpoint_id.is_local() {
            &mut self.local
        } else {
            &mut self.machine
        };
        server.board.remove(server.client_id);
        server.client_id += 1;
        server.generation += 1;
        server.surface_active = false;
        server.terminal = None;
        server.outbox.clear();
        server
            .inbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        server.force_snapshot = true;
        server.sent_focus = None;
        let (inbox, generation) = (server.inbox.clone(), server.generation);
        server.render();
        self.endpoints.insert(
            endpoint_id.clone(),
            Capture(inbox),
            generation,
            negotiation(),
            false,
        );
    }

    // ------------------------------------------------------------ checks

    /// Let every queue drain and the dock's gap and a tick pass.
    fn settle(&mut self) {
        if self.shell().overlay.is_some() {
            self.toggle_modal();
        }
        let mut quiet = 0;
        for _ in 0..80 {
            let mut moved = false;
            while self.local.process_one() {
                moved = true;
            }
            while self.machine.process_one() {
                moved = true;
            }
            while self.deliver_one(false) {
                moved = true;
            }
            while self.deliver_one(true) {
                moved = true;
            }
            self.advance(Duration::from_millis(40));
            if moved || self.local.inbox_len() > 0 || self.machine.inbox_len() > 0 {
                quiet = 0;
            } else {
                quiet += 1;
                if quiet >= 4 {
                    break;
                }
            }
        }
    }

    /// I5: the board shows the placement over a frame that does not make
    /// room for it (checked only once the server read all the client sent).
    fn check_step(&mut self) {
        if self.local.inbox_len() > 0 {
            return;
        }
        let Some(placement) = self.local.shown_placement() else {
            return;
        };
        let Some(drawn) = self
            .state
            .shell
            .as_ref()
            .and_then(|shell| shell.browser_dock.drawn.clone())
        else {
            return;
        };
        let area = Rect::new(
            placement.area.x,
            placement.area.y,
            placement.area.width,
            placement.area.height,
        );
        let tab = drawn.tab_id.as_deref() == Some(placement.tab_id.as_str());
        let over_text = !tab
            || match placement.kind {
                // Browser view draws no pane text
                BrowserPlacementKind::Full => drawn.full.is_empty() || area != drawn.full,
                _ => drawn.band.is_empty() || area.intersects(drawn.pane),
            };
        let note = |kind: &str| {
            format!(
                "{kind} board {:?} {:?} {} over drawn {:?}",
                placement.kind,
                placement.area,
                placement.tab_id,
                (
                    &drawn.tab_id,
                    drawn.band,
                    drawn.full,
                    drawn.pane,
                    drawn.surface
                )
            )
        };
        if trace() && (over_text || drawn.surface.0 > drawn.pane.width) {
            eprintln!("STEP CHECK {}", note(if over_text { "I5a" } else { "I5b" }));
        }
        if over_text {
            self.failures.i5a += 1;
            self.failures.note(note("I5a"));
        } else if placement.kind != BrowserPlacementKind::Full
            && (drawn.surface.0 > drawn.pane.width || drawn.surface.1 > drawn.pane.height)
        {
            self.failures.i5b += 1;
            self.failures.note(note("I5b"));
        }
    }

    fn check_settled(&mut self, label: &str) {
        if trace() {
            let shell = self.state.shell.as_ref().expect("shell");
            eprintln!(
                "SETTLED {label}: client active local={} snapshot focus {:?} rev {:?}; server local focus {} rev {} active {}; drawn {:?}; pending {}",
                shell.active_endpoint_id.is_local(),
                shell.snapshot.as_ref().and_then(|s| s.focused_tab_id.clone()),
                shell.snapshot.as_ref().map(|s| s.revision),
                self.local.focused,
                self.local.rev,
                self.local.surface_active,
                shell.browser_dock.drawn,
                self.pending.is_some()
            );
        }
        let drawn = self
            .state
            .shell
            .as_ref()
            .and_then(|shell| shell.browser_dock.drawn.clone());
        let modal = self
            .state
            .shell
            .as_ref()
            .is_some_and(|shell| shell.overlay.is_some());
        let shown = self.local.shown_placement();
        let local_active = self.endpoints.active_id().is_local();
        let Some(drawn) = drawn.filter(|_| self.pending.is_none()) else {
            self.failures.unsettled += 1;
            self.failures.note(format!("{label}: unsettled"));
            return;
        };
        let server = if local_active {
            &self.local
        } else {
            &self.machine
        };
        let pty = server.pty[&server.focused];
        let focused = server.focused.clone();
        let wants_window =
            local_active && !modal && (!drawn.band.is_empty() || !drawn.full.is_empty());
        if shown.is_some() != wants_window {
            self.failures.i1 += 1;
            self.failures.note(format!(
                "{label}: I1 shown={} band={:?} full={:?}",
                shown.is_some(),
                drawn.band,
                drawn.full
            ));
        }
        if let Some(placement) = shown.as_ref() {
            if !area_matches(placement, &drawn) {
                self.failures.i2 += 1;
                self.failures.note(format!(
                    "{label}: I2 area={:?} band={:?} full={:?}",
                    placement.area, drawn.band, drawn.full
                ));
            }
            if placement.tab_id != focused {
                self.failures.i4 += 1;
                self.failures.note(format!(
                    "{label}: I4 board tab {} focused {focused}",
                    placement.tab_id
                ));
            }
        }
        if pty != (drawn.pane.width, drawn.pane.height) || drawn.surface != pty {
            self.failures.i3 += 1;
            self.failures.note(format!(
                "{label}: I3 pty={pty:?} pane={:?} surface={:?}",
                drawn.pane, drawn.surface
            ));
        }
        if drawn.tab_id.as_deref() != Some(focused.as_str()) {
            self.failures.i4 += 1;
            self.failures.note(format!(
                "{label}: I4 drawn tab {:?} focused {focused}",
                drawn.tab_id
            ));
        }
    }

    fn run(&mut self, steps: u32) {
        for step in 0..steps {
            let roll = self.rng.below(100);
            if roll < 30 {
                self.user_action();
            } else if roll < 65 {
                let machine = self.rng.chance(30);
                for _ in 0..=self.rng.below(3) {
                    let server = if machine {
                        &mut self.machine
                    } else {
                        &mut self.local
                    };
                    server.process_one();
                }
            } else {
                let machine = self.rng.chance(30);
                for _ in 0..=self.rng.below(3) {
                    self.deliver_one(machine);
                }
            }
            let by = Duration::from_millis(self.rng.below(30));
            self.advance(by);
            self.check_step();
            if step % 20 == 19 {
                self.settle();
                self.check_settled(&format!("step {step}"));
            }
        }
        self.settle();
        self.check_settled("end");
    }
}

/// The shown area is the drawn band right of its divider (below a title
/// row, if any) or the drawn Browser view's area.
fn area_matches(
    placement: &crate::api::schema::BrowserPlacementSpec,
    drawn: &super::super::DrawnDock,
) -> bool {
    let area = Rect::new(
        placement.area.x,
        placement.area.y,
        placement.area.width,
        placement.area.height,
    );
    match placement.kind {
        BrowserPlacementKind::Full => !drawn.full.is_empty() && area == drawn.full,
        _ => {
            let band = drawn.band;
            !band.is_empty()
                && area.x == band.x + 1
                && area.width + 1 == band.width
                && area.y >= band.y
                && area.bottom() == band.bottom()
        }
    }
}

fn stress(seeds: std::ops::Range<u64>, steps: u32) -> Vec<(Scenario, Failures)> {
    let mut totals: Vec<(Scenario, Failures)> = SCENARIOS
        .iter()
        .map(|scenario| (*scenario, Failures::default()))
        .collect();
    for seed in seeds {
        let scenario = SCENARIOS[(seed % SCENARIOS.len() as u64) as usize];
        // every scenario runs in both layouts
        let spaces = (seed / SCENARIOS.len() as u64) % 2 == 1;
        let mut harness = Harness::new(seed, scenario, spaces);
        harness.check_settled(&format!("seed {seed} setup"));
        harness.run(steps);
        let total = &mut totals
            .iter_mut()
            .find(|(s, _)| *s == scenario)
            .expect("scenario")
            .1;
        let f = &harness.failures;
        total.unsettled += f.unsettled;
        total.i1 += f.i1;
        total.i2 += f.i2;
        total.i3 += f.i3;
        total.i4 += f.i4;
        total.i5a += f.i5a;
        total.i5b += f.i5b;
        for note in &f.first {
            if total.first.len() < 4 {
                total.first.push(format!("seed {seed}: {note}"));
            }
        }
    }
    totals
}

fn report(totals: &[(Scenario, Failures)]) -> String {
    let mut text =
        String::from("scenario                    unsettled  I1  I2  I3  I4  I5a  I5b(steps)\n");
    for (scenario, f) in totals {
        text.push_str(&format!(
            "{:<27} {:>9} {:>3} {:>3} {:>3} {:>3} {:>4} {:>4}\n",
            format!("{scenario:?}"),
            f.unsettled,
            f.i1,
            f.i2,
            f.i3,
            f.i4,
            f.i5a,
            f.i5b
        ));
        for note in &f.first {
            text.push_str(&format!("    {note}\n"));
        }
    }
    text
}

/// 200 fixed seeds (25 per scenario, 12 or 13 in each sidebar layout), 60
/// steps each:
/// after every settle the board, the drawn frame and the PTY agree, and at
/// no step does the board show the window over text (I5a; I5b is reported).
#[test]
fn fork_smoke_the_dock_stays_in_sync_under_fast_switching() {
    let totals = stress(0..200, 60);
    let text = report(&totals);
    eprintln!("{text}");
    let failed: u32 = totals.iter().map(|(_, f)| f.total_settled() + f.i5a).sum();
    assert_eq!(failed, 0, "\n{text}");
}

/// Scripted runs (no random steps): one interleaving each, checked like a
/// stress settle. `seed` picks the layout (even: tabs, odd: spaces).
fn scripted(seed: u64) -> Harness {
    let mut harness = Harness::new(seed, Scenario::LocalSwitch, seed % 2 == 1);
    harness.check_settled("setup");
    assert_eq!(
        harness.failures.total_settled(),
        0,
        "{:?}",
        harness.failures
    );
    harness
}

fn assert_settled_clean(harness: &mut Harness, label: &str) {
    harness.check_settled(label);
    assert_eq!(
        harness.failures.total_settled(),
        0,
        "{label}: {:?}",
        harness.failures
    );
}

/// F1/F2 (map R1): Local (tab A docked) → the saved machine (no dock) →
/// Local. Each activation sent the size of the layout being left; after
/// each, the shown tab's PTY is the drawn pane rect again.
#[test]
fn a_machine_round_trip_leaves_each_tab_the_size_its_layout_draws() {
    for seed in [0, 1] {
        let mut harness = scripted(seed);
        let machine = harness.machine_id.clone();
        let local_reports = harness.local.resizes.len();
        harness.activate(machine, None, false);
        // until the machine's surface is projected, no size report goes
        // anywhere but the activation's own
        for _ in 0..5 {
            while harness.local.process_one() {}
            while harness.machine.process_one() {}
            while harness.deliver_one(false) {}
            harness.advance(Duration::from_millis(40));
        }
        assert!(
            harness.pending.is_some(),
            "the machine is not projected yet"
        );
        assert_eq!(harness.machine.resizes.len(), 1, "the activation's own");
        assert_eq!(harness.local.resizes.len(), local_reports);
        harness.settle();
        assert_settled_clean(&mut harness, "on the machine");
        harness.click_local_tab("tab_a");
        harness.settle();
        assert_settled_clean(&mut harness, "back on Local");
    }
}

/// F1/F2 (map R1, "Local too"): a Local tab click while a machine
/// activation is pending replaces it with the size of the layout shown;
/// the clicked (undocked) tab still ends at its own size.
#[test]
fn a_local_click_during_a_machine_activation_sizes_the_clicked_tab() {
    for seed in [0, 1] {
        let mut harness = scripted(seed);
        let machine = harness.machine_id.clone();
        harness.activate(machine, None, false);
        harness.machine.process_one();
        harness.click_local_tab("tab_b");
        harness.settle();
        assert_settled_clean(&mut harness, "tab B after the replaced activation");
        harness.click_local_tab("tab_a");
        harness.settle();
        assert_settled_clean(&mut harness, "tab A");
    }
}

/// F2 (map R4): a tab's first browser page adds its view strip (spaces
/// layout): the pane loses a row, and the PTY follows with one report.
#[test]
fn a_tab_s_first_page_resizes_its_pty_once() {
    let mut harness = scripted(1);
    harness.click_local_tab("tab_b");
    harness.settle();
    assert_settled_clean(&mut harness, "tab B");
    let before = harness.local.resizes.len();
    let rows = harness.local.pty["tab_b"].1;
    harness.local.pages.insert("tab_b".into(), 1);
    // `browser.get` polls on its own cadence (2 s or 15 s): let 16 s pass
    for _ in 0..16 {
        harness.advance(Duration::from_secs(1));
        harness.settle();
    }
    assert_settled_clean(&mut harness, "after the strip appeared");
    let reported = &harness.local.resizes[before..];
    assert_eq!(reported.len(), 1, "{reported:?}");
    assert_eq!(reported[0].1, rows - 1);
}

/// F2 (map R4): a dock opened before the first frame (no composed size to
/// compare with) reports no size itself; the loop's check reports the
/// narrower surface once it runs.
#[test]
fn a_dock_opened_before_the_first_frame_still_narrows_the_pty() {
    let mut harness = scripted(0);
    harness.click_local_tab("tab_b");
    harness.settle();
    assert_settled_clean(&mut harness, "tab B");
    let wide = harness.local.pty["tab_b"];
    harness.shell().last_composed_size = None;
    let mut outcome = ClientShellInput::default();
    harness
        .shell()
        .toggle_browser_dock_for_focused_tab(&mut outcome);
    assert!(!outcome.resize, "no composed size to compare with");
    harness.finish(outcome, None);
    harness.settle();
    assert_settled_clean(&mut harness, "docked");
    assert!(harness.local.pty["tab_b"].0 < wide.0);
}

/// F4 (map R5): Local's connection comes back to the same server boot (the
/// server forgot the placement with the old connection): the client places
/// it again.
#[test]
fn a_same_boot_reconnect_places_the_dock_again() {
    let mut harness = scripted(0);
    assert!(harness.local.shown_placement().is_some());
    harness.reconnect_local();
    harness.settle();
    assert_settled_clean(&mut harness, "after the reconnect");
}

/// F3 (map R2): back from the machine, Local's surface turns active before
/// any Local frame is drawn: the window must not come back over the
/// machine's frame (no band) with the placement Local had before.
#[test]
fn a_return_to_local_shows_the_window_only_once_the_client_places_it() {
    let mut harness = scripted(0);
    let machine = harness.machine_id.clone();
    harness.activate(machine, None, false);
    harness.settle();
    assert!(harness.local.shown_placement().is_none());
    harness.click_local_tab("tab_a");
    // Local reads the activation (its surface turns active); nothing of
    // Local has been drawn yet
    while harness.local.process_one() {}
    let drawn = harness.shell().browser_dock.drawn.clone();
    assert_eq!(
        drawn.and_then(|drawn| drawn.tab_id).as_deref(),
        Some("tab_m1"),
        "the machine's frame is still on screen"
    );
    assert!(
        harness.local.shown_placement().is_none(),
        "the window waits for Local's placement"
    );
    harness.settle();
    assert_settled_clean(&mut harness, "back on Local");
}

/// F5 (map R3): a switch into the docked tab shows the window only over a
/// frame of that tab drawn with room for it, never over the tab left (its
/// text under the window) while the new tab's surface is on its way.
#[test]
fn a_switch_into_the_docked_tab_shows_the_window_only_over_its_frame() {
    for seed in [0, 1] {
        let mut harness = scripted(seed);
        harness.click_local_tab("tab_b");
        harness.settle();
        assert_settled_clean(&mut harness, "tab B");
        harness.click_local_tab("tab_a");
        // the server takes the focus and answers: the snapshot arrives, the
        // surface after it
        while harness.local.process_one() {}
        while let Some(message) = harness.local.outbox.front() {
            if matches!(message, ServerMessage::PaneSurface(_)) {
                break;
            }
            harness.deliver_one(false);
        }
        while harness.local.process_one() {}
        harness.check_step();
        assert_eq!(harness.failures.i5a, 0, "the window is over tab B's frame");
        harness.settle();
        assert_settled_clean(&mut harness, "tab A");
    }
}

fn trace() -> bool {
    std::env::var_os("DOCK_SYNC_TRACE").is_some()
}

fn brief_client(message: &ClientMessage) -> String {
    match message {
        ClientMessage::ClientShellEndpointRequest { request, .. } => {
            let request: Option<Request> = serde_json::from_str(request).ok();
            request.map_or("request?".into(), |r| {
                let method = format!("{:?}", r.method);
                format!("req {} {}", r.id, &method[..method.len().min(60)])
            })
        }
        ClientMessage::ClientShellResize { surface_size, .. } => {
            format!("resize {}x{}", surface_size.cols, surface_size.rows)
        }
        other => {
            let text = format!("{other:?}");
            text[..text.len().min(50)].to_owned()
        }
    }
}

fn brief_server(message: &ServerMessage) -> String {
    match message {
        ServerMessage::PaneSurface(surface) => format!(
            "surface rev {} {}x{}",
            surface.projection_revision, surface.frame.width, surface.frame.height
        ),
        ServerMessage::ClientShellEndpointResponseChunk {
            request_id, data, ..
        } => {
            let text = String::from_utf8_lossy(data);
            format!("answer {request_id} {}", &text[..text.len().min(70)])
        }
        ServerMessage::EndpointControl { kind, data } => {
            match endpoint::decode_endpoint_control(kind, data) {
                Ok(endpoint::EndpointControlMessage::Snapshot(snapshot)) => format!(
                    "snapshot rev {} focus {:?}",
                    snapshot.revision, snapshot.focused_tab_id
                ),
                _ => format!("control {kind}"),
            }
        }
        _ => "other".into(),
    }
}

/// A debugging aid and the wider sweep: `DOCK_SYNC_SEEDS=200..1000` runs
/// those seeds (default `0..1`); `DOCK_SYNC_TRACE=1` prints every message,
/// user action and settle.
#[test]
#[ignore = "a debugging aid: DOCK_SYNC_SEEDS=a..b DOCK_SYNC_TRACE=1"]
fn dock_sync_seed_sweep() {
    let seeds = std::env::var("DOCK_SYNC_SEEDS").unwrap_or_default();
    let (start, end) = seeds
        .split_once("..")
        .and_then(|(start, end)| Some((start.parse().ok()?, end.parse().ok()?)))
        .unwrap_or((0, 1));
    let totals = stress(start..end, 60);
    let text = report(&totals);
    eprintln!("{text}");
    let failed: u32 = totals.iter().map(|(_, f)| f.total_settled() + f.i5a).sum();
    assert_eq!(failed, 0, "\n{text}");
}

/// The loop's check before every wait when nothing changed
/// (`reconcile_after_event`: one `layout()` and a compare), with the dock
/// open, at 1 and 16 tabs. Run by `just bench-render-scale`.
#[test]
#[ignore = "a profile: just bench-render-scale"]
fn dock_sync_reconcile_render_scale_profile() {
    for count in [1_usize, 16] {
        let names: Vec<String> = (0..count)
            .map(|index| {
                if index == 0 {
                    "tab_a".to_owned()
                } else {
                    format!("tab_{index}")
                }
            })
            .collect();
        let tabs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut harness = Harness::with_local_tabs(0, Scenario::LocalSwitch, false, &tabs);
        harness.check_settled("setup");
        assert_eq!(harness.failures.total_settled(), 0);
        let reports = harness.local.resizes.len();
        let mut samples = Vec::new();
        for _ in 0..40 {
            let started = Instant::now();
            for _ in 0..250 {
                crate::client::loop_arms::reconcile_after_event(
                    &mut harness.state,
                    &mut harness.endpoints,
                    &mut harness.pending,
                    harness.now,
                )
                .expect("reconcile");
            }
            samples.push(started.elapsed().as_nanos() / 250);
        }
        assert_eq!(
            harness.local.inbox_len(),
            0,
            "nothing changed: nothing sent"
        );
        assert_eq!(harness.local.resizes.len(), reports);
        samples.sort_unstable();
        println!(
            "dock reconcile tabs={count} median_ns={} p95_ns={}",
            samples[samples.len() / 2],
            samples[samples.len() * 95 / 100]
        );
    }
}
