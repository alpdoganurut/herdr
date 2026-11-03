//! The settings overlay's `browser` section (fork): the herdr+ Browser's
//! toggles, integrations, look and actions, from `browser.settings`.
//!
//! Every toggle row goes to the active server as `browser.settings.set`
//! (the server writes its own config file and reloads it; the reply
//! refreshes the section), so a remote server's own setup changes. The
//! colour row and the MCP row cycle through choices on ↵ or →. The MCP row
//! edits user files on the server host (an MCP registration) — that happens
//! on ↵, the user's explicit act, and rows that do so say `(on <machine>)`
//! for a remote endpoint. The two agent rows (steering, hiding agents' own
//! browsers) apply only while Settings → Agents wraps claude / codex; the
//! shell hook lives there too. `fix all` runs the setup steps for every
//! failing check (`browser.fix`); `open browser` / `stop browser` start or
//! stop the default profile. Fork (session header): the rows the tabs
//! sidebar's Browser row menu had follow (that row is gone): the
//! companion's focus / start / restart / stop, the focused tab's browser
//! dock, focus the herdr+ window, open the Browser overlay — from the
//! shell's last `browser.get`, synced on entry and on every pull while the
//! section is open. Below the rows, under a rule, the facts: the live state
//! (the old row's glyph and status), the status line, the browser, the
//! helper, the extension, the MCP registrations — each with ✓/✗ and a
//! short reason from the server's checks.

use super::render::{display_width, put_text};
use super::*;
use crate::api::schema::{
    BrowserCheckInfo, BrowserFixParams, BrowserProfileTarget, BrowserSettingsInfo,
    BrowserSettingsSetParams, BrowserStopParams, EmptyParams, Method,
};
use crossterm::event::KeyModifiers;

/// The activity colour presets ↵/→ cycle through.
pub(super) const COLOR_PRESETS: [&str; 5] = ["#aa6eff", "#00c8ff", "#5fd3a0", "#ffb86b", "#ff7aa8"];
/// The MCP row's cycle: both → claude → codex → none.
const MCP_CYCLE: [&[&str]; 4] = [&["claude", "codex"], &["claude"], &["codex"], &[]];
/// How soon the section pulls again while the server is checking or fixing.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(900);

pub(super) const ROW_ENABLED: usize = 0;
pub(super) const ROW_ACTIVITY: usize = 1;
pub(super) const ROW_DASHBOARD: usize = 2;
pub(super) const ROW_COLOR: usize = 3;
pub(super) const ROW_STEER: usize = 4;
pub(super) const ROW_HIDE_NATIVE: usize = 5;
pub(super) const ROW_MCP: usize = 6;
pub(super) const ROW_FIX: usize = 7;
pub(super) const ROW_OPEN_STOP: usize = 8;
pub(super) const ROW_INSTALL: usize = 9;
pub(super) const ROWS: usize = 10;

/// The section's state inside the settings overlay: its own copy of the
/// `browser.settings` record (the renderer sees the overlay, not the shell
/// state) and where it came from.
#[derive(Debug, Default)]
pub(super) struct ClientBrowserSettings {
    pub(super) info: Option<BrowserSettingsInfo>,
    pub(super) loading: bool,
    /// The active server does not advertise `browser.settings`.
    pub(super) unsupported: bool,
    /// The active endpoint's label and whether it is remote (file edits
    /// happen there).
    pub(super) endpoint_label: String,
    pub(super) remote: bool,
    /// Pull again at this time (the server is checking or fixing).
    pub(super) poll_at: Option<std::time::Instant>,
    /// A one-line note under the facts (`copied …`).
    pub(super) note: Option<String>,
    /// Fork (session header): the browser's live state (the old Browser
    /// row) and the actions its menu had, from the shell's `browser.get`.
    pub(super) live: Option<super::browser::BrowserRow>,
    pub(super) actions: Vec<LiveAction>,
}

/// Fork (session header): one of the rows after `ROWS`, the Browser row
/// menu's actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LiveAction {
    pub(super) label: String,
    pub(super) action: ClientContextMenuAction,
}

/// The live rows for the browser's state: the companion's items (a local
/// endpoint), the focused tab's dock where the browser docks, focus the
/// herdr+ window while it runs, and the Browser overlay.
pub(super) fn live_actions(
    live: Option<&super::browser::BrowserRow>,
    local: bool,
    window: bool,
) -> Vec<LiveAction> {
    let mut actions = Vec::new();
    let mut push = |label: String, action| actions.push(LiveAction { label, action });
    if let Some(live) = live.filter(|_| local) {
        if let Some(state) = live.companion.as_deref() {
            for (label, action, _) in super::browser::companion_menu_items(state) {
                push(
                    label.to_lowercase(),
                    ClientContextMenuAction::BrowserCompanion(action),
                );
            }
        }
        if let Some(docked) = live.dock {
            push(
                format!(
                    "browser dock on this tab: {}",
                    if docked { "on" } else { "off" }
                ),
                ClientContextMenuAction::BrowserToggleDock,
            );
        }
        if (live.companion.is_none() || window) && live.running {
            push(
                "focus the browser window".into(),
                ClientContextMenuAction::BrowserFocusWindow,
            );
        }
        // Beside the companion, the herdr+ window's own profile (without
        // it, `open browser` / `stop browser` above is that profile).
        if live.companion.is_some() && window {
            push(
                if live.running {
                    "stop the browser window".into()
                } else {
                    "start the browser window".into()
                },
                ClientContextMenuAction::BrowserToggleProfile,
            );
        }
    }
    push(
        "open the Browser overlay…".into(),
        ClientContextMenuAction::BrowserOpenOverlay,
    );
    actions
}

/// The whole command the install row copies.
pub(super) fn install_command() -> &'static str {
    "herdr browser install-chromium <Chromium.app>"
}

/// `#rrggbb` as a terminal colour.
pub(super) fn hex_color(text: &str) -> Option<ratatui::style::Color> {
    let hex = text.trim().strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(ratatui::style::Color::Rgb(
        channel(0)?,
        channel(2)?,
        channel(4)?,
    ))
}

/// The colour after `current` in the presets (a custom value sits in front
/// of the presets, so the first → from it reaches the first preset).
pub(super) fn next_color(current: &str) -> &'static str {
    let current = current.trim().to_ascii_lowercase();
    match COLOR_PRESETS.iter().position(|c| *c == current) {
        Some(index) => COLOR_PRESETS[(index + 1) % COLOR_PRESETS.len()],
        None => COLOR_PRESETS[0],
    }
}

/// The agents after `current` in the MCP cycle.
pub(super) fn next_mcp_agents(current: &[String]) -> Vec<String> {
    let has = |agent: &str| current.iter().any(|a| a == agent);
    let index = match (has("claude"), has("codex")) {
        (true, true) => 0,
        (true, false) => 1,
        (false, true) => 2,
        (false, false) => 3,
    };
    MCP_CYCLE[(index + 1) % MCP_CYCLE.len()]
        .iter()
        .map(|a| (*a).to_string())
        .collect()
}

/// Failing checks a fix exists for.
pub(super) fn fixable_issues(info: &BrowserSettingsInfo) -> usize {
    info.checks.iter().filter(|c| !c.ok && c.fixable).count()
}

/// The extension update is failing but withheld because agents use the
/// browser: how many (the check recorded it; an older server's check has
/// no count, so `None`).
fn extension_update_withheld(info: &BrowserSettingsInfo) -> Option<u32> {
    check(info, "extension")
        .filter(|c| !c.ok && !c.fixable && c.waits_for_agents > 0)
        .map(|c| c.waits_for_agents)
}

fn check<'a>(info: &'a BrowserSettingsInfo, id: &str) -> Option<&'a BrowserCheckInfo> {
    info.checks.iter().find(|c| c.id == id)
}

impl ClientShellState {
    fn browser_settings(&self) -> Option<&ClientBrowserSettings> {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::Settings(settings)) => Some(&settings.browser),
            _ => None,
        }
    }

    fn browser_settings_mut(&mut self) -> Option<&mut ClientBrowserSettings> {
        match self.overlay.as_mut() {
            Some(ClientShellOverlay::Settings(settings)) => Some(&mut settings.browser),
            _ => None,
        }
    }

    fn browser_section_open(&self) -> bool {
        matches!(
            self.overlay,
            Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
                section: ClientSettingsSection::Browser,
                ..
            }))
        )
    }

    /// Rows in the browser section: the ten rows and the live ones once a
    /// record is there.
    pub(super) fn browser_section_rows(&self) -> usize {
        match self.browser_settings() {
            Some(ClientBrowserSettings {
                info: Some(_),
                actions,
                ..
            }) => ROWS + actions.len(),
            _ => 0,
        }
    }

    /// Fork (session header): copy the browser's live state into the open
    /// section (on entry and after each `browser.get` reply).
    pub(super) fn sync_browser_settings_live(&mut self) {
        if !self.browser_section_open() {
            return;
        }
        let live = self.browser_row();
        let local = self.active_endpoint_id.is_local();
        let window = self
            .browser
            .info
            .as_ref()
            .is_some_and(|info| !info.window_off);
        let actions = live_actions(live.as_ref(), local, window);
        if let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() {
            settings.browser.live = live;
            settings.browser.actions = actions;
            if settings.browser.info.is_some() {
                let rows = ROWS + settings.browser.actions.len();
                settings.selected = settings.selected.min(rows - 1);
            }
        }
    }

    /// Entering the section: note the endpoint, pull the record (or say the
    /// server has no `browser.settings`).
    pub(super) fn enter_browser_section(&mut self, outcome: &mut ClientShellInput) {
        let label = self.active_endpoint_label().to_string();
        let remote = !self.active_endpoint_id.is_local();
        let supported =
            self.supports_endpoint_method(&Method::BrowserSettings(EmptyParams::default()));
        if let Some(browser) = self.browser_settings_mut() {
            browser.endpoint_label = label;
            browser.remote = remote;
            browser.unsupported = !supported;
            browser.note = None;
            browser.poll_at = None;
        }
        if supported {
            self.pull_browser_settings(outcome);
        }
        self.sync_browser_settings_live();
        self.refresh_browser();
        outcome.repaint = true;
    }

    fn pull_browser_settings(&mut self, outcome: &mut ClientShellInput) {
        if self.browser_settings().is_some_and(|b| b.loading) {
            return;
        }
        if self.push_endpoint_method_with_kind(
            Method::BrowserSettings(EmptyParams::default()),
            PendingEndpointKind::BrowserSettings,
            outcome,
        ) {
            if let Some(browser) = self.browser_settings_mut() {
                browser.loading = true;
                browser.poll_at = None;
            }
        }
    }

    /// The tick loop: pull again when the server was checking or fixing.
    pub(crate) fn tick_browser_settings(
        &mut self,
        now: std::time::Instant,
        outcome: &mut ClientShellInput,
    ) {
        if !self.browser_section_open() {
            return;
        }
        if self.browser_settings().is_some_and(|b| b.loading) {
            return;
        }
        let due = self
            .browser_settings()
            .and_then(|b| b.poll_at)
            .is_some_and(|at| at <= now);
        if due {
            self.pull_browser_settings(outcome);
        }
    }

    /// When the tick loop must wake for the section.
    pub(crate) fn next_browser_settings_deadline(&self) -> Option<std::time::Instant> {
        if !self.browser_section_open() {
            return None;
        }
        self.browser_settings()
            .filter(|b| !b.loading)
            .and_then(|b| b.poll_at)
    }

    /// A `browser.settings*` reply (or its failure) reaches the open section.
    pub(super) fn handle_browser_settings_endpoint_result(
        &mut self,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let now = std::time::Instant::now();
        let Some(browser) = self.browser_settings_mut() else {
            return (false, Vec::new());
        };
        browser.loading = false;
        match result {
            Ok(crate::api::schema::ResponseResult::BrowserSettings { settings }) => {
                browser.poll_at =
                    (settings.checking || settings.fixing).then_some(now + POLL_INTERVAL);
                browser.info = Some(settings);
            }
            Ok(_) => {
                browser.info = None;
                self.set_endpoint_error("endpoint returned an unexpected browser settings result");
            }
            // `endpoint_busy` (a pull was in flight): try again shortly, the
            // section keeps what it shows. A refusal (a bad value, a config
            // write that failed) is shown under the facts; the poll then
            // brings the real state back.
            Err(err) => {
                if err.code.as_deref() != Some("endpoint_busy") {
                    browser.note = Some(format!("not applied: {}", err.message));
                }
                browser.poll_at = Some(now + POLL_INTERVAL);
            }
        }
        if let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() {
            if settings.section == ClientSettingsSection::Browser {
                let rows = ROWS + settings.browser.actions.len();
                settings.selected = settings.selected.min(rows - 1);
            }
        }
        // The Browser row and the overlay follow a start/stop or a fix.
        self.refresh_browser();
        (true, Vec::new())
    }

    fn set_browser_setting(
        &mut self,
        key: &str,
        value: serde_json::Value,
        outcome: &mut ClientShellInput,
    ) {
        if self.push_endpoint_method_with_kind(
            Method::BrowserSettingsSet(BrowserSettingsSetParams {
                key: key.to_string(),
                value,
            }),
            PendingEndpointKind::BrowserSettingsSet,
            outcome,
        ) {
            if let Some(browser) = self.browser_settings_mut() {
                browser.loading = true;
                browser.note = None;
            }
        }
        outcome.repaint = true;
    }

    /// Enter or a click on the section's current row.
    pub(super) fn apply_browser_choice(&mut self, selected: usize, outcome: &mut ClientShellInput) {
        let Some(info) = self.browser_settings().and_then(|b| b.info.clone()) else {
            return;
        };
        if selected >= ROWS {
            self.apply_browser_live_action(selected - ROWS, outcome);
            return;
        }
        match selected {
            ROW_ENABLED => self.set_browser_setting("enabled", (!info.enabled).into(), outcome),
            ROW_ACTIVITY => {
                self.set_browser_setting("show_activity", (!info.show_activity).into(), outcome)
            }
            ROW_DASHBOARD => {
                self.set_browser_setting("pin_dashboard", (!info.pin_dashboard).into(), outcome)
            }
            ROW_COLOR => self.set_browser_setting(
                "activity_color",
                next_color(&info.activity_color).into(),
                outcome,
            ),
            // Steering only: wrapping claude / codex is Settings → Agents.
            ROW_STEER => {
                self.set_browser_setting("steer_agents", (!info.steer_agents).into(), outcome)
            }
            ROW_HIDE_NATIVE => self.set_browser_setting(
                "disable_native_browser",
                (!info.disable_native_browser).into(),
                outcome,
            ),
            ROW_MCP => self.set_browser_setting(
                "mcp_agents",
                serde_json::Value::Array(
                    next_mcp_agents(&info.mcp_agents)
                        .into_iter()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
                outcome,
            ),
            ROW_FIX => {
                if info.fixing || fixable_issues(&info) == 0 {
                    return;
                }
                if self.push_endpoint_method_with_kind(
                    Method::BrowserFix(BrowserFixParams { ids: Vec::new() }),
                    PendingEndpointKind::BrowserFix,
                    outcome,
                ) {
                    if let Some(browser) = self.browser_settings_mut() {
                        browser.loading = true;
                    }
                }
                outcome.repaint = true;
            }
            ROW_OPEN_STOP => {
                let (method, kind) = if info.running {
                    (
                        Method::BrowserStop(BrowserStopParams {
                            profile: Some(info.profile.clone()),
                            all: false,
                        }),
                        PendingEndpointKind::BrowserStop,
                    )
                } else {
                    (
                        Method::BrowserStart(BrowserProfileTarget {
                            profile: Some(info.profile.clone()),
                        }),
                        PendingEndpointKind::BrowserStart,
                    )
                };
                if self.push_endpoint_method_with_kind(method, kind, outcome) {
                    if let Some(browser) = self.browser_settings_mut() {
                        // the start/stop reply is a browser.get; the section pulls itself
                        browser.poll_at = Some(std::time::Instant::now() + POLL_INTERVAL);
                    }
                }
                outcome.repaint = true;
            }
            ROW_INSTALL => {
                outcome
                    .actions
                    .push(ClientShellAction::ClipboardWrite(install_command().into()));
                self.show_copy_feedback(std::time::Instant::now());
                if let Some(browser) = self.browser_settings_mut() {
                    browser.note = Some(format!(
                        "copied `{}` — paste it in a shell with the path of a Chromium build",
                        install_command()
                    ));
                }
                outcome.repaint = true;
            }
            _ => {}
        }
    }

    /// Fork (session header): a live row: what the Browser row's menu item
    /// did (the dock acts on the focused tab).
    fn apply_browser_live_action(&mut self, index: usize, outcome: &mut ClientShellInput) {
        let Some(action) = self
            .browser_settings()
            .and_then(|b| b.actions.get(index))
            .map(|row| row.action)
        else {
            return;
        };
        let (profile, running) = self
            .browser_settings()
            .and_then(|b| b.live.as_ref())
            .map(|live| (live.profile.clone(), live.running))
            .unwrap_or_else(|| ("main".into(), false));
        match action {
            ClientContextMenuAction::BrowserToggleDock => {
                self.toggle_browser_dock_for_focused_tab(outcome);
            }
            action => self.activate_browser_context_action(profile, running, action, outcome),
        }
        self.refresh_browser();
        self.sync_browser_settings_live();
        outcome.repaint = true;
    }

    /// The section's own keys, ahead of the overlay's: → cycles the colour
    /// and the MCP rows (elsewhere → switches sections).
    pub(super) fn route_browser_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if code != KeyCode::Right || !modifiers.is_empty() {
            return false;
        }
        let selected = match self.overlay.as_ref() {
            Some(ClientShellOverlay::Settings(settings))
                if settings.section == ClientSettingsSection::Browser
                    && settings.browser.info.is_some() =>
            {
                settings.selected
            }
            _ => return false,
        };
        if selected != ROW_COLOR && selected != ROW_MCP {
            return false;
        }
        self.apply_browser_choice(selected, outcome);
        true
    }

    /// Every row of the browser section acts on a click.
    pub(super) fn browser_click_applies(&self) -> bool {
        self.browser_section_open()
    }
}

/// A row: the rect styled, then text segments with their own colours.
fn draw_row(
    buffer: &mut Buffer,
    rect: Rect,
    segments: &[(String, Option<ratatui::style::Color>)],
    selected: bool,
    own_marker: bool,
    palette: &Palette,
) {
    let base = if selected {
        Style::default()
            .fg(panel_contrast_fg(palette))
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.text).bg(palette.panel_bg)
    };
    buffer.set_style(rect, base);
    let mut x = rect.x;
    // an action row carries its own ▸; the selection bar marks it
    let marker = if selected && !own_marker {
        " ▸ "
    } else {
        "   "
    };
    put_text(buffer, x, rect.y, rect.width, marker, base);
    x = x.saturating_add(display_width(marker));
    for (text, color) in segments {
        if x >= rect.right() {
            break;
        }
        let style = match color {
            // a colour of its own only on an unselected row (the selection
            // bar stays readable)
            Some(color) if !selected => base.fg(*color),
            _ => base,
        };
        put_text(
            buffer,
            x,
            rect.y,
            rect.right().saturating_sub(x),
            text,
            style,
        );
        x = x.saturating_add(display_width(text));
    }
}

fn draw_fact(
    buffer: &mut Buffer,
    area: Rect,
    y: u16,
    label: &str,
    segments: &[(String, Option<ratatui::style::Color>)],
    palette: &Palette,
) {
    if y >= area.bottom() {
        return;
    }
    let dim = Style::default().fg(palette.overlay1).bg(palette.panel_bg);
    let text = Style::default().fg(palette.text).bg(palette.panel_bg);
    put_text(
        buffer,
        area.x,
        y,
        area.width,
        &format!("   {label:<11}"),
        dim,
    );
    let mut x = area.x.saturating_add(14);
    // a fact never runs past the panel: what does not fit ends in `…`
    for (segment, color) in clip_segments(segments, area.right().saturating_sub(x)) {
        if x >= area.right() {
            break;
        }
        let style = color.map_or(text, |c| text.fg(c));
        put_text(
            buffer,
            x,
            y,
            area.right().saturating_sub(x),
            &segment,
            style,
        );
        x = x.saturating_add(display_width(&segment));
    }
}

/// `segments` cut to `width` display cells, measured and cut per grapheme
/// cluster (so what is measured is what is cut). When they do not all fit,
/// one cell is kept for the `…` up front, the first segment — the mark —
/// is never cut (only the mark when nothing else fits, nothing when not
/// even that), a cut segment ends in `…` and nothing follows it; empty
/// segments are dropped.
pub(super) fn clip_segments(
    segments: &[(String, Option<ratatui::style::Color>)],
    width: u16,
) -> Segments {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    let cell = |g: &str| UnicodeWidthStr::width(g).min(u16::MAX as usize) as u16;
    let measure = |text: &str| {
        text.graphemes(true)
            .fold(0u16, |acc, g| acc.saturating_add(cell(g)))
    };
    let widths: Vec<u16> = segments.iter().map(|(text, _)| measure(text)).collect();
    let total = widths.iter().fold(0u16, |acc, w| acc.saturating_add(*w));
    let keep = |i: usize| (segments[i].0.clone(), segments[i].1);
    if total <= width {
        return (0..segments.len())
            .filter(|i| widths[*i] > 0)
            .map(keep)
            .collect();
    }
    let mut out: Segments = Vec::new();
    let budget = width.saturating_sub(1); // the ellipsis cell, reserved
    let mut used: u16 = 0;
    for (i, (text, color)) in segments.iter().enumerate() {
        let wanted = widths[i];
        if i == 0 {
            // the mark: whole or not at all; alone when nothing else can follow
            if wanted > width {
                return out;
            }
            if wanted > 0 {
                out.push(keep(0));
            }
            if wanted >= budget {
                return out;
            }
            used = wanted;
            continue;
        }
        if wanted == 0 {
            continue;
        }
        if used.saturating_add(wanted) <= budget {
            out.push(keep(i));
            used = used.saturating_add(wanted);
            continue;
        }
        let head = budget.saturating_sub(used);
        let mut kept = String::new();
        let mut kept_width: u16 = 0;
        for g in text.graphemes(true) {
            let w = cell(g);
            if kept_width.saturating_add(w) > head {
                break;
            }
            kept.push_str(g);
            kept_width = kept_width.saturating_add(w);
        }
        kept.push('…');
        out.push((kept, *color));
        return out;
    }
    out
}

fn mark(ok: bool, palette: &Palette) -> (String, Option<ratatui::style::Color>) {
    if ok {
        ("✓".into(), Some(palette.green))
    } else {
        ("✗".into(), Some(palette.red))
    }
}

/// The row labels (no colours), for tests and the renderer.
pub(super) fn row_labels(info: &BrowserSettingsInfo, remote_label: Option<&str>) -> Vec<String> {
    let on = |value: bool| if value { "on" } else { "off" };
    let remote = |text: String| match remote_label {
        Some(label) => format!("{text} (on {label})"),
        None => text,
    };
    let fix = if info.fixing {
        "▸ fixing…".to_string()
    } else {
        match (fixable_issues(info), extension_update_withheld(info)) {
            (0, Some(agents)) => format!(
                "▸ fix all (extension waits for {agents} agent{})",
                if agents == 1 { "" } else { "s" }
            ),
            (0, None) if info.checks.iter().any(|c| !c.ok) => {
                "▸ fix all (nothing fixable here)".to_string()
            }
            (0, None) => "▸ fix all (nothing to fix)".to_string(),
            (1, _) => "▸ fix all (1 issue)".to_string(),
            (n, _) => format!("▸ fix all ({n} issues)"),
        }
    };
    let has = |agent: &str| info.mcp_agents.iter().any(|a| a == agent);
    vec![
        format!("browser: {}", on(info.enabled)),
        format!(
            "show activity (groups, glow, cursor): {}",
            on(info.show_activity)
        ),
        format!("pinned dashboard: {}", on(info.pin_dashboard)),
        format!("activity colour  {} ■", info.activity_color),
        format!(
            "agents prefer herdr's browser (when wrapped): {}",
            on(info.steer_agents)
        ),
        format!(
            "hide agents' own browsers (when wrapped): {}",
            on(info.disable_native_browser)
        ),
        remote(format!(
            "MCP for: claude {}  codex {}",
            if has("claude") { "✓" } else { "✗" },
            if has("codex") { "✓" } else { "✗" }
        )),
        remote(fix),
        if info.running {
            "stop browser".to_string()
        } else {
            "open browser".to_string()
        },
        remote("install herdr+ Browser from a Chromium build… (↵ copies the command)".to_string()),
    ]
}

/// The section: a title, the rows, a rule, the facts.
pub(super) fn render_browser_section(
    buffer: &mut Buffer,
    area: Rect,
    settings: &ClientSettingsOverlay,
    palette: &Palette,
    hits: &mut Vec<(Rect, usize)>,
) {
    let title_style = Style::default()
        .fg(palette.text)
        .bg(palette.panel_bg)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(palette.overlay1).bg(palette.panel_bg);
    let browser = &settings.browser;
    put_text(buffer, area.x, area.y, area.width, "browser", title_style);
    put_text(
        buffer,
        area.x,
        area.y + 1,
        area.width,
        "herdr+ Browser for agents; ↵ toggles or runs, → changes a choice",
        dim,
    );
    let Some(info) = browser.info.as_ref() else {
        let message = if browser.unsupported {
            " browser settings unavailable on this server (an older herdr)"
        } else if browser.loading {
            " loading browser settings…"
        } else {
            " browser settings unavailable on this server"
        };
        put_text(buffer, area.x, area.y + 3, area.width, message, dim);
        return;
    };
    let remote_label = browser.remote.then_some(browser.endpoint_label.as_str());
    let labels = row_labels(info, remote_label);
    for (index, label) in labels.iter().enumerate() {
        let y = area.y + 3 + index as u16;
        if y >= area.bottom() {
            break;
        }
        let rect = Rect::new(area.x, y, area.width, 1);
        let segments: Vec<(String, Option<ratatui::style::Color>)> = match index {
            ROW_COLOR => {
                let swatch = hex_color(&info.activity_color);
                vec![
                    (label.trim_end_matches('■').to_string(), None),
                    ("■".into(), swatch),
                ]
            }
            ROW_MCP => {
                let has = |agent: &str| info.mcp_agents.iter().any(|a| a == agent);
                let mut segments = vec![
                    ("MCP for: claude ".to_string(), None),
                    mark(has("claude"), palette),
                ];
                segments.push(("  codex ".into(), None));
                segments.push(mark(has("codex"), palette));
                if let Some(remote) = remote_label {
                    segments.push((format!(" (on {remote})"), None));
                }
                segments
            }
            ROW_FIX => vec![(label.clone(), Some(palette.accent))],
            _ => vec![(label.clone(), None)],
        };
        draw_row(
            buffer,
            rect,
            &segments,
            index == settings.selected,
            index == ROW_FIX,
            palette,
        );
        hits.push((rect, index));
    }
    // Fork (session header): the Browser row menu's actions.
    for (offset, row) in browser.actions.iter().enumerate() {
        let index = ROWS + offset;
        let y = area.y + 3 + index as u16;
        if y >= area.bottom() {
            break;
        }
        let rect = Rect::new(area.x, y, area.width, 1);
        draw_row(
            buffer,
            rect,
            &[(row.label.clone(), None)],
            index == settings.selected,
            false,
            palette,
        );
        hits.push((rect, index));
    }
    let rule_y = area.y + 3 + (labels.len() + browser.actions.len()) as u16;
    if rule_y < area.bottom() {
        put_text(
            buffer,
            area.x,
            rule_y,
            area.width,
            &format!(
                "  {}",
                "─".repeat(usize::from(area.width).saturating_sub(4))
            ),
            Style::default().fg(palette.surface0).bg(palette.panel_bg),
        );
    }
    let mut facts = facts(info, palette);
    // Fork (session header): the live state leads (the old Browser row).
    if let Some(live) = browser.live.as_ref() {
        facts.insert(
            0,
            (
                "now",
                vec![
                    (
                        live.state.glyph().to_string(),
                        Some(live.state.color(palette, live.active)),
                    ),
                    (format!(" {}", live.status), None),
                ],
            ),
        );
    }
    for (offset, (label, segments)) in facts.iter().enumerate() {
        draw_fact(
            buffer,
            area,
            rule_y + 1 + offset as u16,
            label,
            segments,
            palette,
        );
    }
    if let Some(note) = browser.note.as_deref() {
        let y = rule_y + 1 + facts.len() as u16;
        if y < area.bottom() {
            put_text(buffer, area.x, y, area.width, &format!("   {note}"), dim);
        }
    }
}

type Segments = Vec<(String, Option<ratatui::style::Color>)>;

/// The facts block: one line per check, ✓/✗ and the reason.
pub(super) fn facts(
    info: &BrowserSettingsInfo,
    palette: &Palette,
) -> Vec<(&'static str, Segments)> {
    let dim = Some(palette.overlay1);
    // the mark first: a long reason is clipped at the right, never the verdict
    let line = |id: &str| -> Segments {
        match check(info, id) {
            Some(c) => vec![mark(c.ok, palette), (format!(" {}", c.detail), None)],
            None if info.checking => vec![("checking…".into(), dim)],
            None => vec![("—".into(), dim)],
        }
    };
    let mcp: Segments = {
        let one = |id: &str, name: &str| -> Segments {
            match check(info, id) {
                Some(c) => {
                    let mut out = vec![(format!("{name} "), None), mark(c.ok, palette)];
                    if !c.ok {
                        let reason = c
                            .detail
                            .split_once(": ")
                            .map_or(c.detail.as_str(), |(_, r)| r);
                        out.push((format!(" {reason}"), dim));
                    }
                    out
                }
                None => vec![(format!("{name} "), None), ("—".into(), dim)],
            }
        };
        let mut out = one("mcp_claude", "claude");
        out.push((" · ".into(), dim));
        out.extend(one("mcp_codex", "codex"));
        out
    };
    let mut facts = vec![
        ("status", vec![(info.status.clone(), None)]),
        ("browser", line("executable")),
        ("helper", line("helper")),
        ("extension", line("extension")),
        // Fork (companion launcher): the companion app herdr runs.
        ("companion", line("companion_app")),
        ("MCP", mcp),
    ];
    if !info.fixes.is_empty() {
        let mut segments: Segments = Vec::new();
        for (i, fix) in info.fixes.iter().enumerate() {
            if i > 0 {
                segments.push((" · ".into(), dim));
            }
            segments.push((format!("{} ", fix.id), None));
            segments.push(mark(fix.ok, palette));
            if !fix.ok {
                segments.push((format!(" {}", fix.detail), dim));
            }
        }
        facts.push(("last fix", segments));
    }
    facts
}
