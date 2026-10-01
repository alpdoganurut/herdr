//! The settings overlay's `browser` section (fork): the herdr+ Browser's
//! toggles, integrations, look and actions, from `browser.settings`.
//!
//! Every toggle row goes to the active server as `browser.settings.set`
//! (the server writes its own config file and reloads it; the reply
//! refreshes the section), so a remote server's own setup changes. The
//! colour row and the MCP row cycle through choices on ↵ or →. The MCP and
//! shell-hook rows edit user files on the server host (an MCP registration,
//! `~/.zshrc`) — that happens on ↵, the user's explicit act, and rows that
//! do so say `(on <machine>)` for a remote endpoint. `fix all` runs the
//! setup steps for every failing check (`browser.fix`); `open browser` /
//! `stop browser` start or stop the default profile. Below the rows, under
//! a rule, the facts: the status line, the browser, the helper, the
//! extension, the MCP registrations, the shell hook — each with ✓/✗ and a
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
pub(super) const ROW_HOOK: usize = 7;
pub(super) const ROW_FIX: usize = 8;
pub(super) const ROW_OPEN_STOP: usize = 9;
pub(super) const ROW_INSTALL: usize = 10;
pub(super) const ROWS: usize = 11;

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

    /// Rows in the browser section: the eleven rows once a record is there.
    pub(super) fn browser_section_rows(&self) -> usize {
        match self.browser_settings() {
            Some(ClientBrowserSettings { info: Some(_), .. }) => ROWS,
            _ => 0,
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
                settings.selected = settings.selected.min(ROWS - 1);
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
            ROW_STEER => {
                // One row, both keys, one server-side write: on when both are on.
                let on = !(info.steer_agents && info.wrap_agents);
                self.set_browser_setting("steer_wrap", on.into(), outcome);
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
            ROW_HOOK => self.set_browser_setting("shell_hook", (!info.shell_hook).into(), outcome),
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
    for (segment, color) in segments {
        if x >= area.right() {
            break;
        }
        let style = color.map_or(text, |c| text.fg(c));
        put_text(buffer, x, y, area.right().saturating_sub(x), segment, style);
        x = x.saturating_add(display_width(segment));
    }
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
        match fixable_issues(info) {
            0 => "▸ fix all (nothing to fix)".to_string(),
            1 => "▸ fix all (1 issue)".to_string(),
            n => format!("▸ fix all ({n} issues)"),
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
            "agents use herdr's browser (steer + wrap): {}",
            on(info.steer_agents && info.wrap_agents)
        ),
        format!(
            "hide agents' own browsers: {}",
            on(info.disable_native_browser)
        ),
        remote(format!(
            "MCP for: claude {}  codex {}",
            if has("claude") { "✓" } else { "✗" },
            if has("codex") { "✓" } else { "✗" }
        )),
        remote(format!("shell hook in ~/.zshrc: {}", on(info.shell_hook))),
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
    let rule_y = area.y + 3 + labels.len() as u16;
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
    let facts = facts(info, palette);
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
    let hook: Segments = line("shell_hook");
    let mut facts = vec![
        ("status", vec![(info.status.clone(), None)]),
        ("browser", line("executable")),
        ("helper", line("helper")),
        ("extension", line("extension")),
        ("MCP", mcp),
        ("shell hook", hook),
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
