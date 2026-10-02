//! The settings overlay's `agents` section (fork): wrapping plain `claude` /
//! `codex` launches in herdr+ panes, from `agents.settings`.
//!
//! Rows act on ↵ or a click and go to the active server as
//! `agents.settings.set` (the server writes its own `[agents]` config and
//! reloads it; the reply refreshes the section): the master `wrap` switch,
//! the two contributions (the agent tools, the instructions paragraph) —
//! dimmed and inert while the wrap is off — and the instructions file
//! (built-in ↔ `agents.md` in the server's config dir). The status row shows
//! the shell hook and whether `claude` / `codex` are on the server's PATH.
//!
//! The shell hook's `[fix]` edits `~/.zshrc`, so it is the one two-step row
//! in Settings: ↵ (or a click) on `▸ fix` only *arms* it — the section then
//! shows the exact line it adds and two buttons, `↵ edit ~/.zshrc` and
//! `esc cancel`. Only the confirm button (or ↵ while armed) sends
//! `agents.fix { ids: ["shell_hook"], confirm: true }`; esc, a click
//! elsewhere, another key or leaving the section disarms. The client never
//! edits a file itself.

use super::render::{display_width, put_text};
use super::*;
use crate::api::schema::{
    AgentsCheckInfo, AgentsCheckState, AgentsFixParams, AgentsSettingsInfo,
    AgentsSettingsSetParams, AgentsWrapSource, EmptyParams, Method,
};
use crossterm::event::KeyModifiers;

pub(super) const ROW_WRAP: usize = 0;
pub(super) const ROW_TOOLS: usize = 1;
pub(super) const ROW_INSTRUCTIONS: usize = 2;
pub(super) const ROW_FILE: usize = 3;
pub(super) const ROW_STATUS: usize = 4;
pub(super) const ROWS: usize = 5;
/// The armed `[fix]`'s buttons, as choice hits past the rows (the mouse
/// handler routes them before it selects a row).
pub(super) const HIT_CONFIRM: usize = 100;
pub(super) const HIT_CANCEL: usize = 101;

const SHELL_HOOK: &str = crate::api::schema::agent_wrap::check::SHELL_HOOK;

/// The section's state inside the settings overlay: its own copy of the
/// `agents.settings` record (the renderer sees the overlay, not the shell
/// state), where it came from, and the armed `[fix]`.
#[derive(Debug, Default)]
pub(super) struct ClientAgentsSettings {
    pub(super) info: Option<AgentsSettingsInfo>,
    pub(super) loading: bool,
    /// The active server does not advertise `agents.settings`.
    pub(super) unsupported: bool,
    /// The active endpoint's label and whether it is remote (the hook fix
    /// edits that machine's `~/.zshrc`).
    pub(super) endpoint_label: String,
    pub(super) remote: bool,
    /// `▸ fix` was pressed: the preview and the confirm buttons show, and
    /// only a confirm sends `agents.fix`.
    pub(super) armed: bool,
    /// An `agents.fix` is in flight.
    pub(super) fixing: bool,
    /// A one-line note under the hints (a refusal, the fix's outcome).
    pub(super) note: Option<String>,
}

fn check<'a>(info: &'a AgentsSettingsInfo, id: &str) -> Option<&'a AgentsCheckInfo> {
    info.checks.iter().find(|c| c.id == id)
}

/// The shell hook can be fixed (the server decides; a current hook never is).
pub(super) fn hook_fixable(info: &AgentsSettingsInfo) -> bool {
    check(info, SHELL_HOOK).is_some_and(|c| c.fixable && c.state != AgentsCheckState::Ok)
}

fn state_mark(state: AgentsCheckState) -> &'static str {
    match state {
        AgentsCheckState::Ok => "✓",
        AgentsCheckState::Unknown => "?",
        AgentsCheckState::Outdated | AgentsCheckState::Missing | AgentsCheckState::Absent => "✗",
    }
}

/// `shell hook ✓ · claude ✓ · codex ✗` (an outdated hook says so).
fn status_summary(info: &AgentsSettingsInfo) -> String {
    let one = |id: &str, name: &str| match check(info, id) {
        Some(c) if c.state == AgentsCheckState::Outdated => format!("{name} ✗ outdated"),
        Some(c) => format!("{name} {}", state_mark(c.state)),
        None => format!("{name} —"),
    };
    format!(
        "{} · {} · {}",
        one(SHELL_HOOK, "shell hook"),
        one(crate::api::schema::agent_wrap::check::CLAUDE, "claude"),
        one(crate::api::schema::agent_wrap::check::CODEX, "codex"),
    )
}

/// The status row's action part, if any.
fn fix_label(settings: &ClientAgentsSettings, info: &AgentsSettingsInfo) -> Option<String> {
    if settings.fixing {
        return Some("▸ fixing…".into());
    }
    if !hook_fixable(info) {
        return None;
    }
    Some(match settings.remote {
        true => format!("▸ fix (edits ~/.zshrc on {})", settings.endpoint_label),
        false => "▸ fix (edits ~/.zshrc)".into(),
    })
}

/// The row labels (no colours), for tests and the renderer.
pub(super) fn row_labels(
    settings: &ClientAgentsSettings,
    info: &AgentsSettingsInfo,
) -> Vec<String> {
    let on = |value: bool| if value { "on" } else { "off" };
    let source = match info.wrap_source {
        AgentsWrapSource::BrowserLegacy => "   (from [browser] wrap_agents)",
        AgentsWrapSource::Agents | AgentsWrapSource::Default | AgentsWrapSource::Unknown => "",
    };
    let file = if info.instructions_detail.is_empty() {
        if info.instructions_file.is_empty() {
            "built-in".to_string()
        } else {
            info.instructions_file.clone()
        }
    } else {
        info.instructions_detail.clone()
    };
    let file_hint = if info.instructions_file.is_empty() {
        "(↵ uses agents.md in herdr's config dir)"
    } else {
        "(↵ back to built-in)"
    };
    let mut status = status_summary(info);
    if let Some(fix) = fix_label(settings, info) {
        status = format!("{status}      {fix}");
    }
    vec![
        format!(
            "wrap claude / codex in herdr+ panes: {}{source}",
            on(info.wrap)
        ),
        format!("  add agent tools (notify): {}", on(info.tools)),
        format!(
            "  add herdr+ instructions to the system prompt: {}",
            on(info.instructions)
        ),
        format!("  instructions file: {file}   {file_hint}"),
        status,
    ]
}

/// The hint lines under the checks.
pub(super) fn hint_lines(info: &AgentsSettingsInfo) -> [String; 3] {
    [
        "changes apply on the next launch; running agents keep what they launched with".into(),
        "per launch: claude --no-herdr · codex --no-herdr · HERDR_NO_WRAP=1 · command claude"
            .into(),
        format!(
            "browser steering: [browser] steer_agents = {} (applies while wrapped)",
            if info.steer_browser { "on" } else { "off" }
        ),
    ]
}

impl ClientShellState {
    fn agents_settings(&self) -> Option<&ClientAgentsSettings> {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::Settings(settings)) => Some(&settings.agents),
            _ => None,
        }
    }

    fn agents_settings_mut(&mut self) -> Option<&mut ClientAgentsSettings> {
        match self.overlay.as_mut() {
            Some(ClientShellOverlay::Settings(settings)) => Some(&mut settings.agents),
            _ => None,
        }
    }

    fn agents_section_open(&self) -> bool {
        matches!(
            self.overlay,
            Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
                section: ClientSettingsSection::Agents,
                ..
            }))
        )
    }

    /// Rows in the agents section: the five rows once a record is there.
    pub(super) fn agents_section_rows(&self) -> usize {
        match self.agents_settings() {
            Some(ClientAgentsSettings { info: Some(_), .. }) => ROWS,
            _ => 0,
        }
    }

    /// Entering the section: note the endpoint, pull the record (or say the
    /// server has no `agents.settings`).
    pub(super) fn enter_agents_section(&mut self, outcome: &mut ClientShellInput) {
        let label = self.active_endpoint_label().to_string();
        let remote = !self.active_endpoint_id.is_local();
        let supported =
            self.supports_endpoint_method(&Method::AgentsSettings(EmptyParams::default()));
        if let Some(agents) = self.agents_settings_mut() {
            agents.endpoint_label = label;
            agents.remote = remote;
            agents.unsupported = !supported;
            agents.armed = false;
            agents.note = None;
        }
        if supported {
            self.pull_agents_settings(outcome);
        }
        outcome.repaint = true;
    }

    fn pull_agents_settings(&mut self, outcome: &mut ClientShellInput) {
        if self.agents_settings().is_some_and(|a| a.loading) {
            return;
        }
        if self.push_endpoint_method_with_kind(
            Method::AgentsSettings(EmptyParams::default()),
            PendingEndpointKind::AgentsSettings,
            outcome,
        ) {
            if let Some(agents) = self.agents_settings_mut() {
                agents.loading = true;
            }
        }
    }

    fn set_agents_setting(
        &mut self,
        key: &str,
        value: serde_json::Value,
        outcome: &mut ClientShellInput,
    ) {
        if self.push_endpoint_method_with_kind(
            Method::AgentsSettingsSet(AgentsSettingsSetParams {
                key: key.to_string(),
                value,
            }),
            PendingEndpointKind::AgentsSettingsSet,
            outcome,
        ) {
            if let Some(agents) = self.agents_settings_mut() {
                agents.loading = true;
                agents.note = None;
            }
        }
        outcome.repaint = true;
    }

    /// Enter or a click on the section's current row. The fix row only arms.
    pub(super) fn apply_agents_choice(&mut self, selected: usize, outcome: &mut ClientShellInput) {
        let Some(agents) = self.agents_settings() else {
            return;
        };
        let Some(info) = agents.info.clone() else {
            return;
        };
        let fixing = agents.fixing;
        match selected {
            ROW_WRAP => self.set_agents_setting("wrap", (!info.wrap).into(), outcome),
            // inert while the wrap is off: they only shape wrapped launches
            ROW_TOOLS if info.wrap => {
                self.set_agents_setting("tools", (!info.tools).into(), outcome)
            }
            ROW_INSTRUCTIONS if info.wrap => {
                self.set_agents_setting("instructions", (!info.instructions).into(), outcome)
            }
            ROW_FILE => {
                let next = if info.instructions_file.is_empty() {
                    "file"
                } else {
                    "default"
                };
                self.set_agents_setting("instructions_file", next.into(), outcome)
            }
            ROW_STATUS if !fixing && hook_fixable(&info) => {
                if let Some(agents) = self.agents_settings_mut() {
                    agents.armed = true;
                    agents.note = None;
                }
                outcome.repaint = true;
            }
            _ => {}
        }
    }

    /// The confirm: the one place `agents.fix` is sent, always with
    /// `confirm: true` for the shell hook only.
    fn confirm_agents_fix(&mut self, outcome: &mut ClientShellInput) {
        let ready = self
            .agents_settings()
            .is_some_and(|a| a.armed && !a.fixing && a.info.as_ref().is_some_and(hook_fixable));
        if let Some(agents) = self.agents_settings_mut() {
            agents.armed = false;
        }
        outcome.repaint = true;
        if !ready {
            return;
        }
        if self.push_endpoint_method_with_kind(
            Method::AgentsFix(AgentsFixParams {
                ids: vec![SHELL_HOOK.to_string()],
                confirm: true,
            }),
            PendingEndpointKind::AgentsFix,
            outcome,
        ) {
            if let Some(agents) = self.agents_settings_mut() {
                agents.fixing = true;
                agents.note = None;
            }
        }
    }

    fn disarm_agents_fix(&mut self) -> bool {
        match self.agents_settings_mut() {
            Some(agents) if agents.armed => {
                agents.armed = false;
                true
            }
            _ => false,
        }
    }

    fn agents_fix_armed(&self) -> bool {
        self.agents_section_open() && self.agents_settings().is_some_and(|a| a.armed)
    }

    /// The section's keys, ahead of the overlay's: while the fix is armed,
    /// ↵ / space confirm, esc cancels, and any other key cancels and then
    /// does what it always does.
    pub(super) fn route_agents_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !self.agents_fix_armed() {
            return false;
        }
        if modifiers.is_empty() && matches!(code, KeyCode::Enter | KeyCode::Char(' ')) {
            self.confirm_agents_fix(outcome);
            return true;
        }
        self.disarm_agents_fix();
        outcome.repaint = true;
        code == KeyCode::Esc
    }

    /// A press in the settings overlay while the fix is armed: the confirm
    /// and cancel buttons act here; anything else disarms and goes on.
    pub(super) fn route_agents_click(
        &mut self,
        choice: Option<usize>,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !self.agents_fix_armed() {
            return false;
        }
        match choice {
            Some(HIT_CONFIRM) => {
                self.confirm_agents_fix(outcome);
                true
            }
            Some(HIT_CANCEL) => {
                self.disarm_agents_fix();
                outcome.repaint = true;
                true
            }
            _ => {
                self.disarm_agents_fix();
                outcome.repaint = true;
                false
            }
        }
    }

    /// Every row of the agents section acts on a click.
    pub(super) fn agents_click_applies(&self) -> bool {
        self.agents_section_open()
    }

    /// An `agents.*` reply (or its failure) reaches the open section.
    pub(super) fn handle_agents_settings_endpoint_result(
        &mut self,
        kind: PendingEndpointKind,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let Some(agents) = self.agents_settings_mut() else {
            return (false, Vec::new());
        };
        let mut refresh = false;
        match (kind, result) {
            (
                PendingEndpointKind::AgentsSettings | PendingEndpointKind::AgentsSettingsSet,
                Ok(crate::api::schema::ResponseResult::AgentsSettings { info }),
            ) => {
                agents.loading = false;
                // a check that changed under an armed fix must be confirmed anew
                if agents.armed && !hook_fixable(&info) {
                    agents.armed = false;
                }
                agents.info = Some(info);
            }
            (
                PendingEndpointKind::AgentsFix,
                Ok(crate::api::schema::ResponseResult::AgentsFix { results }),
            ) => {
                agents.fixing = false;
                let hook = results.iter().find(|c| c.id == SHELL_HOOK);
                agents.note = hook.map(|c| {
                    if c.state == AgentsCheckState::Ok {
                        format!("shell hook fixed: {}", c.detail)
                    } else {
                        format!("shell hook not fixed: {}", c.detail)
                    }
                });
                if let Some(info) = agents.info.as_mut() {
                    for result in &results {
                        match info.checks.iter_mut().find(|c| c.id == result.id) {
                            Some(existing) => *existing = result.clone(),
                            None => info.checks.push(result.clone()),
                        }
                    }
                }
                refresh = true;
            }
            (PendingEndpointKind::AgentsFix, Ok(_)) => {
                agents.fixing = false;
                agents.note = Some("endpoint returned an unexpected agents.fix result".into());
            }
            (PendingEndpointKind::AgentsFix, Err(err)) => {
                agents.fixing = false;
                agents.note = Some(format!("not fixed: {}", err.message));
                refresh = true;
            }
            (_, Ok(_)) => {
                agents.loading = false;
                agents.info = None;
                self.set_endpoint_error("endpoint returned an unexpected agents settings result");
            }
            // A refusal (a bad value, a config write that failed) is shown
            // under the hints; the pull brings the real state back.
            (kind, Err(err)) => {
                agents.loading = false;
                if err.code.as_deref() != Some("endpoint_busy") {
                    agents.note = Some(format!("not applied: {}", err.message));
                }
                refresh = matches!(kind, PendingEndpointKind::AgentsSettingsSet);
            }
        }
        if let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() {
            if settings.section == ClientSettingsSection::Agents {
                settings.selected = settings.selected.min(ROWS - 1);
            }
        }
        let mut deferred = ClientShellInput::default();
        if refresh && self.agents_section_open() {
            self.pull_agents_settings(&mut deferred);
        }
        (true, deferred.actions)
    }
}

/// A row: the rect styled, then the text; a dimmed row is drawn in a muted
/// colour (inert). `accent_from` (a byte offset into `label`) draws the
/// rest of the label in the accent, the status row's `▸ fix …`.
fn draw_row(
    buffer: &mut Buffer,
    rect: Rect,
    label: &str,
    selected: bool,
    dimmed: bool,
    accent_from: Option<usize>,
    palette: &Palette,
) {
    let base = if selected {
        Style::default()
            .fg(panel_contrast_fg(palette))
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else if dimmed {
        Style::default().fg(palette.overlay0).bg(palette.panel_bg)
    } else {
        Style::default().fg(palette.text).bg(palette.panel_bg)
    };
    buffer.set_style(rect, base);
    let marker = if selected { " ▸ " } else { "   " };
    put_text(buffer, rect.x, rect.y, rect.width, marker, base);
    let x = rect.x.saturating_add(display_width(marker));
    let (head, tail) = match accent_from.filter(|at| label.is_char_boundary(*at)) {
        Some(at) => label.split_at(at),
        None => (label, ""),
    };
    put_text(
        buffer,
        x,
        rect.y,
        rect.right().saturating_sub(x),
        head,
        base,
    );
    if !tail.is_empty() {
        let tx = x.saturating_add(display_width(head));
        if tx < rect.right() {
            // the accent only on an unselected row (the selection bar stays readable)
            let style = if selected {
                base
            } else {
                base.fg(palette.accent)
            };
            put_text(
                buffer,
                tx,
                rect.y,
                rect.right().saturating_sub(tx),
                tail,
                style,
            );
        }
    }
}

fn draw_button(buffer: &mut Buffer, rect: Rect, text: &str, style: Style) {
    buffer.set_style(rect, style);
    let width = display_width(text).min(rect.width);
    put_text(
        buffer,
        rect.x + (rect.width - width) / 2,
        rect.y,
        width,
        text,
        style,
    );
}

/// The section: a title, the rows, the armed fix (preview and buttons), a
/// rule, the checks, the hints and the note.
pub(super) fn render_agents_section(
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
    let text = Style::default().fg(palette.text).bg(palette.panel_bg);
    let agents = &settings.agents;
    put_text(buffer, area.x, area.y, area.width, "agents", title_style);
    put_text(
        buffer,
        area.x,
        area.y + 1,
        area.width,
        "plain claude / codex launched in herdr+ panes; ↵ toggles",
        dim,
    );
    let Some(info) = agents.info.as_ref() else {
        let message = if agents.unsupported {
            " agents settings unavailable on this server (an older herdr)"
        } else if agents.loading {
            " loading agents settings…"
        } else {
            " agents settings unavailable on this server"
        };
        put_text(buffer, area.x, area.y + 3, area.width, message, dim);
        return;
    };
    let labels = row_labels(agents, info);
    let mut y = area.y + 3;
    for (index, label) in labels.iter().enumerate() {
        if y >= area.bottom() {
            return;
        }
        let rect = Rect::new(area.x, y, area.width, 1);
        let dimmed = matches!(index, ROW_TOOLS | ROW_INSTRUCTIONS) && !info.wrap;
        let accent_from = (index == ROW_STATUS).then(|| label.find('▸')).flatten();
        draw_row(
            buffer,
            rect,
            label,
            index == settings.selected,
            dimmed,
            accent_from,
            palette,
        );
        hits.push((rect, index));
        y += 1;
    }
    if agents.armed {
        let preview = info
            .hook_preview
            .as_deref()
            .filter(|preview| !preview.trim().is_empty())
            .unwrap_or("herdr adds its shell hook line to ~/.zshrc");
        let heading = if agents.remote {
            format!("   this edits ~/.zshrc on {}:", agents.endpoint_label)
        } else {
            "   this edits ~/.zshrc:".to_string()
        };
        for line in std::iter::once(heading).chain(preview.lines().map(|l| format!("     {l}"))) {
            if y >= area.bottom() {
                return;
            }
            put_text(buffer, area.x, y, area.width, &line, text);
            y += 1;
        }
        if y < area.bottom() {
            let confirm_label = " ↵ edit ~/.zshrc ";
            let cancel_label = " esc cancel ";
            let confirm_width = display_width(confirm_label).saturating_add(2);
            let cancel_width = display_width(cancel_label).saturating_add(2);
            let x = area.x.saturating_add(3);
            let confirm = Rect::new(x, y, confirm_width.min(area.right().saturating_sub(x)), 1);
            let cancel_x = confirm.right().saturating_add(2);
            let cancel = Rect::new(
                cancel_x,
                y,
                cancel_width.min(area.right().saturating_sub(cancel_x)),
                1,
            );
            draw_button(
                buffer,
                confirm,
                confirm_label,
                Style::default()
                    .fg(panel_contrast_fg(palette))
                    .bg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            );
            draw_button(
                buffer,
                cancel,
                cancel_label,
                Style::default()
                    .fg(palette.text)
                    .bg(palette.surface0)
                    .add_modifier(Modifier::BOLD),
            );
            hits.push((confirm, HIT_CONFIRM));
            hits.push((cancel, HIT_CANCEL));
            y += 1;
        }
    }
    if y >= area.bottom() {
        return;
    }
    put_text(
        buffer,
        area.x,
        y,
        area.width,
        &format!(
            "  {}",
            "─".repeat(usize::from(area.width).saturating_sub(4))
        ),
        Style::default().fg(palette.surface0).bg(palette.panel_bg),
    );
    y += 1;
    for (id, name) in [
        (SHELL_HOOK, "shell hook"),
        (crate::api::schema::agent_wrap::check::CLAUDE, "claude"),
        (crate::api::schema::agent_wrap::check::CODEX, "codex"),
    ] {
        if y >= area.bottom() {
            return;
        }
        put_text(
            buffer,
            area.x,
            y,
            area.width,
            &format!("   {name:<11}"),
            dim,
        );
        let x = area.x.saturating_add(14);
        let width = area.right().saturating_sub(x);
        match check(info, id) {
            Some(c) => {
                let color = if c.state == AgentsCheckState::Ok {
                    palette.green
                } else {
                    palette.red
                };
                put_text(buffer, x, y, width, state_mark(c.state), text.fg(color));
                put_text(
                    buffer,
                    x.saturating_add(1),
                    y,
                    width.saturating_sub(1),
                    &format!(" {}", c.detail),
                    text,
                );
            }
            None => put_text(buffer, x, y, width, "—", dim),
        }
        y += 1;
    }
    for hint in hint_lines(info) {
        if y >= area.bottom() {
            return;
        }
        put_text(buffer, area.x, y, area.width, &format!("   {hint}"), dim);
        y += 1;
    }
    if let Some(note) = agents.note.as_deref() {
        if y < area.bottom() {
            put_text(buffer, area.x, y, area.width, &format!("   {note}"), dim);
        }
    }
}
