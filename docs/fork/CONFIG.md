# herdr+ configuration guide

What you can turn on or off in herdr+, what it starts as, and where to change it.

- **Config file:** `~/.config/herdr/config.toml` (`$XDG_CONFIG_HOME/herdr/config.toml` when that is set; debug builds use `herdr-dev`). `herdr --default-config` prints a commented default file; the full key reference is `docs/next/website/src/data/config-reference.json`.
- **Settings panel:** `prefix+s` (`ctrl+b`, then `s`). Sections: theme, indicators, sound, toasts, integrations, backups, reminders, closed, news, browser, coordinator, agents. Settings rows write the same `config.toml` keys; the browser, coordinator and agents sections ask the server to write its own config and reload it.
- **Reload after a hand edit:** `prefix+shift+r` (`reload_config`).
- **Default column:** **on** = opt-out (it works until you turn it off), **off** = opt-in.

Most fork sidebar features need the `tabs` sidebar layout. It is the default; `[ui] sidebar_layout = "spaces"` goes back to upstream's space and agent panels.

## Agents (`[agents]`)

Wrapping changes apply at the next agent launch; running agents keep what they started with.

| Feature | Default | Where to change | In Settings? |
|---|---|---|---|
| Wrap plain `claude` / `codex` in herdr+ panes (master switch for tools and instructions) | off (unset; falls back to legacy `[browser] wrap_agents`, else off) | `[agents] wrap = true` | Yes, agents → wrap |
| Agent tools (the herdr_agents MCP server) on wrapped launches | off | `[agents] tools = true` (needs `wrap`) | Yes, agents |
| herdr+ instructions paragraph on wrapped launches | off | `[agents] instructions = true` (needs `wrap`) | Yes, agents |
| Custom instructions text | built-in | `[agents] instructions_file = "~/path.md"` (empty = built-in) | Yes, toggles built-in and `agents.md` in the config dir; other paths config only |
| Team roster, purpose and team tools for agents launched in a team group | on | `[agents] team_roster = false` | Yes, agents |
| Agent notices (`agents_notify`, `herdr agent notify`) | on | `[agents] notices = false` | No, config only |
| Program status (OSC 7501 probe; Claude Code 2.1.295+). Screen detection still decides the state. Applied live | on | `[agents] program_status = false` | Yes, agents |
| Context window per agent (sidebar context %) | from session / model | `[agents] context_window = { claude = 1000000 }` | No, shown read-only |

### Handoff and compact (`[agents.handoff]`, Claude only)

Before a Claude agent's context is compacted, herdr has it write a handoff, saves the transcript, types `/compact` and points the new session back at both.

| Feature | Default | Where to change | In Settings? |
|---|---|---|---|
| Handoff feature as a whole | on | `[agents.handoff] enabled = false`. Off also removes the menu item, the `handoff_compact` key and the PreCompact safety net | Yes, agents |
| Automatic trigger at context use | 80 | `[agents.handoff] at_percent = 0..100` (0 = no automatic trigger) | Yes, cycles 60 / 70 / 80 / 90 / off |
| Custom handoff prompt | herdr's own | `[agents.handoff] command = "/my:handoff {path}"` (`{path}` is replaced; appended when missing) | No, shown read-only |
| Resume line after compaction | on | `[agents.handoff] resume = false` | No, config only |

### Suspend and autosuspend

| Feature | Default | Where to change | In Settings? |
|---|---|---|---|
| Suspend / activate an agent by hand | always available | Tab menu "Suspend agent" / "Activate agent", or bind `toggle_agent_suspend` | n/a |
| Autosuspend idle Claude agents (idle and seen, nothing running; a message wakes it) | off | `[agents.autosuspend] after = "off"` or `"<N>m"`, `"<N>h"`, `"<N>d"` (at most 365 days) | Yes, agents, cycles 1h / 4h / 1d / 3d / off |
| Restart an idle agent in place | always available | Tab menu "Restart agent", or bind `restart_agent` | n/a |
| Transcript backups (a copy of each open or suspended agent's native transcript, put back before a resume when the agent deleted its own) | on | `[session] backup_agent_transcripts = false` | Status only (backups section) |
| Resume agents on session restore (upstream) | on | `[session] resume_agents_on_restore = false` | No, config only |

## Sidebar (`[ui]`, `tabs` layout)

| Feature | Default | Where to change | In Settings? |
|---|---|---|---|
| Tabs sidebar layout (needed by the rows below) | on (`"tabs"`; upstream's default is `"spaces"`) | `[ui] sidebar_layout = "spaces"` | Only under `spaces`, as the one-way hint row in coordinator settings that switches to `tabs` |
| Active block (blocked, working, finished agents) | on | `[ui] sidebar_active_agents = false` | Yes, indicators |
| Pinned block | on | `[ui] sidebar_pinned_agents = false` | Yes, indicators |
| Scheduled block (tabs with a reminder) | on | `[ui] sidebar_scheduled_agents = false` | Yes, indicators |
| Suspended-run folding (3+ suspended tabs in a row fold into "N suspended") | on | `[ui] sidebar_collapse_suspended = false` | Yes, indicators ("fold suspended runs") |
| Context use (`NN%` from 75 %) | on | `[ui] sidebar_context_usage = false` | No, config only |
| Sidebar search (type to filter tabs; Esc clears, Enter focuses the first match) | available | Key `sidebar_search` (default `prefix+f`) or click the toolbar field | n/a, not saved |
| Recency filter (`◷` chip: off, 15m, 30m, 1h, 6h, 1d) | off | Click the chip (shift-click or right-click steps back). No config key | n/a, not saved |
| Tab history back / forward | available | The toolbar's `‹` `›` buttons; keys `tab_history_back` (`cmd+[`, `alt+,`), `tab_history_forward` (`cmd+]`, `alt+.`) | n/a |

The search query and recency chip belong to the client only. They are never saved: a restart or a new client lists every tab again.

### Sidebar toolbar

The top row of the tabs sidebar, left to right: `⏶ ‹ ›  ⌕ search  ◷ 1h  ◧  +`. Dim controls are `overlay0`; lit ones take the theme accent. Widths below are the sidebar's (`[ui] sidebar_width`, default 26; `sidebar_min_width` 18).

| Control | What it does | Shown when | Key / setting |
|---|---|---|---|
| `⏶` / `⏷` fold toggle | `⏶` folds every group, `⏷` (once all are folded) expands them all. The group holding the focused tab always stays open. Does nothing while a search or recency filter is on | There is a group that can fold | `toggle_groups_folded` (unbound) |
| `‹` `›` tab history | Back to the previously focused tab, forward again after going back (browser-style). Lit while there is a tab that way; a dim one does nothing | Always, from 24 columns; below that they drop first | `tab_history_back` (`cmd+[`, `alt+,`), `tab_history_forward` (`cmd+]`, `alt+.`), see the note under Keybinds |
| `⌕ search` field | Click to type a filter (Esc clears and leaves, Enter focuses the first match, the query stays). Dim placeholder when empty, accent `⌕` with a query, highlighted with a caret while typing | On toolbar hover, or while the field has the keyboard, holds a query or the recency chip is on; from 20 columns | `sidebar_search` (`prefix+f`) |
| `◷` recency chip | Click steps forward through off, 15m, 30m, 1h, 6h, 1d and round again; shift-click or right-click steps back. Dim `◷` while off, accent `◷ 1h` while on | Same as the search field | None, not saved |
| `◧` notes button | Opens or closes the focused tab's notes pane, without moving the keyboard to it. Lit while that pane is open | Always | `toggle_info_pane` (unbound) |
| `+` | Moves the focused tab to a group by name, creating the group when the name is new | Always | `move_tab_to_group` (unbound) |

## Per-tab toggles (tab menu)

Right-click a tab. These are per tab, not config keys. Important, pin, mute, do not disturb, the reminder and the colour are server session state: they are saved with the tab in `session.json`, so they survive a server restart (session restore) and a live handoff, and move with the tab. Only the notes pane's open or closed state belongs to the client and is never saved.

| Item | Default (new tab) | What it does |
|---|---|---|
| Important | off | Reminds you while its agent waits finished-unseen or blocked (also key `toggle_tab_important`) |
| Pin | off | Lists the tab in the Pinned block |
| Mute notifications | off | No herdr automatic notifications for the tab (toasts, sounds, system/terminal notifications, bells, reminders); agent cards still show |
| Do not disturb | off | Agent messages to the tab are stored quietly instead of typed in, urgent ones are refused, herdr types nothing in and the coordinator's wake-ups are held; your typing is unaffected. Notifications are untouched (that is mute). An agent can also turn it on or off for its own tab and its teammates' tabs (`agents_dnd`, `herdr tab dnd on|off`), never outside its team or on the coordinator's tab; the coordinator can, for any other tab, in your turn. When an agent or the coordinator turned it on, the coordinator's messages still get through |
| remind | none | Scheduled reminder: 5m, 10m, 30m, 1h, 6h, daily. Pick the current one again to turn it off. Picking never focuses the tab |
| Color | none | Color tag (also key `cycle_tab_color`) |
| Info pane | hidden, every start | Shows or hides the tab's notes pane (also key `toggle_info_pane`, or the `◧` toolbar button). Which tabs have it open is client-only: after a restart or in a new client every notes pane starts hidden |

## Notes pane (`[notes]`)

| Feature | Default | Where to change | In Settings? |
|---|---|---|---|
| Notes pane and the agents' notes tools | on | `[notes] enabled = false` (nothing is read or written under `<config dir>/notes/`) | No, config only |
| Notes pane default width | 44 | `[ui] info_pane_width = 28..120`. A width you drag or step is saved per client and wins from then on | No, config only |

## Reminders, toasts and sounds (`[ui]`, `[ui.toast]`, `[ui.sound]`)

| Feature | Default | Where to change | In Settings? |
|---|---|---|---|
| Idle reminders for important tabs | 10 min | `[ui] idle_reminder_minutes = 0..240` (0 = off) | Yes, reminders (0 / 5 / 10 / 15 / 30 / 60) |
| Daily reminder time | `"09:30"` | `[ui] daily_reminder_time = "HH:MM"` | Yes, reminders |
| Toast popups (upstream) | off | `[ui.toast] delivery = "off"`, `"herdr"`, `"terminal"` or `"system"` | Yes, toasts |
| Sticky in-app toasts (stack until dismissed) | off | `[ui.toast.herdr] sticky = true`; `max_stack = 1..20` (default 6) | No, config only |
| Toast position, delay | bottom-right, 1 s | `[ui.toast.herdr] position`; `[ui.toast] delay_seconds = 0..3600` | No, config only |
| Clipboard toast | on | `[ui.toast.clipboard] enabled = false` | No, config only |
| Sounds (upstream) | on | `[ui.sound] enabled = false` | Yes, sound |
| Sound files (finished, needs input, reminder) | default sound (a reminder without `reminder_path` plays the finished or needs-input sound) | `[ui.sound] done_path`, `request_path`, `reminder_path` | Yes, sound (picker: `default` and the sounds in `/System/Library/Sounds`) |
| Per-agent sound | `default` | `[ui.sound.agents] claude = "on"`, `"off"` or `"default"` | No, config only |

## Coordinator (`[coordinator]`)

A server-run Claude agent in a pinned `coordinator` tab that watches other agents and suggests next steps.

| Feature | Default | Where to change | In Settings? |
|---|---|---|---|
| Coordinator | off | `[coordinator] enabled = true` | Yes, coordinator |
| Model | Claude's default | `[coordinator] model = "opus"` (applies at the next launch) | Yes (default / opus / sonnet) |
| Wake-ups per hour / day | 12 / 80 | `[coordinator] cap_hour`, `cap_day` | Yes (4–30 / 20–200 pickers) |
| Notifications | on | `[coordinator] notify = false` | Yes, coordinator |
| Suggestion notifications per day | 6 | `[coordinator] notify_daily_cap` | No, config only |
| Quiet hours for its notifications (a `down` alert still goes out) | none | `[coordinator] quiet_hours = "22:00-08:00"` | No, config only |
| Wake scope | `"opened"` | `[coordinator] wake_scope = "opened"`, `"teams"` or `"all"` | No, config only |
| Dashboard on 127.0.0.1 | port 7718 | `[coordinator] dashboard_port = 7718` (0 = off) | Shown, not editable |
| Periodic check when changes are pending | 60 min | `[coordinator] periodic_minutes` | No, config only |
| Relaunches per hour before reported down | 3 | `[coordinator] relaunch_cap_hour` | No, config only |

## Browser (`[browser]`)

The browser herdr runs for agents: the herdr+ companion browser (`herdr browser setup --companion` installs it; the first page an agent opens starts it; `herdr browser companion status|start|stop|restart|focus|log`, the Browser row's menu and the Browser overlay control it). The herdr+ Chromium window is off unless `[browser] window = true`.

| Feature | Default | Where to change | In Settings? |
|---|---|---|---|
| Browser feature | on | `[browser] enabled = false` | Yes, browser |
| Start with the server | off (lazy) | `[browser] autostart = true` | No, config only |
| Close the browser when the server stops (not on a live handoff) | off (the window survives herdr) | `[browser] stop_with_server = true` | No, config only |
| Activity overlay (tab groups, glow, cursor) | on | `[browser] show_activity = false` | Yes, browser |
| Activity color | `#aa6eff` | `[browser] activity_color = "#rrggbb"` | Yes (5 presets) |
| Pinned dashboard tab | on | `[browser] pin_dashboard = false` | Yes, browser |
| Steer agents to the herdr browser | on | `[browser] steer_agents = false` | Yes, browser |
| Turn off agents' own browsers (while `wrap` is on) | on | `[browser] disable_native_browser = false` | Yes, browser |
| MCP registration | claude and codex | `[browser] mcp_agents = ["claude"]` | Yes (both / claude / codex / none) |
| `eval`, act family, typing into password fields | on, on, off | `[browser] allow_eval`, `allow_act`, `type_into_password_fields` | No, config only |
| Restore tabs on relaunch | on | `[browser] restore_tabs = false` | No, config only |
| Where `open` / `show` go (the companion browser, started on demand) | `"companion"` | `[browser] host = "auto"` (the companion while it runs, else the herdr+ window when it is on) or `"window"` | No, config only |
| Page errors of a tab an agent uses, stored quietly for that Claude agent | `"agent"` | `[browser] page_errors = "off"` | No, config only |
| Start the companion browser with the server (else on the first page an agent opens) | off | `[browser] companion_autostart = true` | No, config only |
| The herdr+ Chromium window (off: agents use the companion; a window profile answers `window_disabled`) | off | `[browser] window = true` | No, config only |

The companion browser is the Electron app in `apps/companion`; `apps/companion/scripts/try.sh` tries it against a throwaway herdr in one command (run it in a plain terminal).

### Browser dock and Browser view (`[ui]`, `[keys]`)

The companion browser can sit inside herdr's window: docked on the right of a tab, or over the whole tab (Browser view). herdr keeps the area and the companion covers it, following the terminal window: it hides while the window moves or resizes and comes back aligned, hides while the window is minimised, hidden or on another Space, and joins a full-screen Space. Clicking the page gives it the keyboard; clicking the terminal (or cmd+esc in the browser) gives it back. macOS, the herdr on this machine, herdr directly in a terminal window (Ghostty's geometry is measured exactly from its padding and titlebar settings; other terminals best effort; inside tmux or ssh the area says why it stays empty).

| Feature | Default | Where to change | In Settings? |
|---|---|---|---|
| Dock the browser on the focused tab | `prefix+shift+b` | `[keys] toggle_browser_dock`; also the `◨` on the pinned Browser row and in the tab's view strip, the tab menu's Dock browser, the Browser row menu's Dock browser here | No, config only |
| Dock width | 45 % of the area right of the sidebar | `[ui] browser_dock_width = 20..80`. Drag the divider (its first column) to resize, double-click it for the default; a dragged width is saved per client | No, config only |
| Browser view (the browser over the whole tab; the panes keep running and keep their size, keys never reach them) | `prefix+shift+z` | `[keys] toggle_browser_view`; also the view strip's `▣ Terminal` / `◎ Browser` and the tab menu's Browser view | No, config only |
| The view strip (`▣ Terminal  ◎ Browser · 3 … ◨`) | `"auto"`: while the tab has pages, the dock or Browser view | `[ui] browser_view_strip = "off"` | No, config only |
| Give the companion the keyboard | unbound | `[keys] focus_browser` | No, config only |

While the browser is not over its area, the area says why: starting (a spinner), not running (`[ Start companion ]`), not installed (the setup command), stopped after 5 crashes (`[ Restart ]`), "Click here once to dock the browser to this window" (several terminal windows could be this one), or the reason it cannot dock here. Tabs with browser pages wear a dim `◎` on their row (lit while an agent uses the browser, `◉` while a page needs you); the detail strip says `◎ 3 pages` and `docked` / `browser view`. In the companion's bar, the count pill (`3 · 12`: pages in this session · in all sessions) lists every session with pages; picking one focuses that herdr tab.

Legacy keys: `wrap_agents` is only read as a fallback for `[agents] wrap` (a Settings change to wrap removes it), and `shell_hook` is inert. The commented default file still says `stop_with_server` is not honoured; it is.

## Updates (`[update]`, upstream)

| Feature | Default | Where to change | In Settings? |
|---|---|---|---|
| Version check | off (upstream's default is on) | `[update] version_check = true` | No, config only |
| Agent detection manifest check | off (upstream's default is on) | `[update] manifest_check = true` | No, config only |

**Both are off on a herdr+ build, and should stay off.** `version_check` polls `herdr.dev/latest.json` in the background, so upstream releases show up as "update available" on a fork binary; the check itself only notifies. Do not follow that prompt: `herdr update` ignores `version_check`, fetches the same upstream manifest, and when upstream's version is newer than the fork's it downloads the upstream binary, verifies its checksum and renames it over the herdr+ binary on disk (`current_exe`). It refuses inside herdr and on Homebrew, mise or Nix installs. Before installing it asks to stop or live-hand-off running servers, and a handoff relaunches them on the upstream binary. After that you run upstream herdr, not herdr+. `manifest_check` downloads `herdr.dev/agent-detection/index.toml`. A remote manifest at the bundled version or newer replaces the fork's bundled detection rules; only the fork's signal rules survive.

## News (`[news]`)

There is an AI news desk. It is off by default (`[news] enabled = false`, with a settings section), and it is outside the scope of this guide.

## config.toml examples

Minimal teammate setup (agents wrapped with tools; the tabs sidebar and no upstream update checks are already the defaults):

```toml
[ui.toast]
delivery = "herdr"

[agents]
wrap = true
tools = true
instructions = true
```

Coordinator on (its pinned row needs the default `tabs` layout):

```toml
[coordinator]
enabled = true
model = "sonnet"
wake_scope = "teams"
quiet_hours = "22:00-08:00"
```

Autosuspend at one day:

```toml
[agents.autosuspend]
after = "1d"
```

Quieter setup (no notes, no handoff, no browser):

```toml
[notes]
enabled = false

[agents.handoff]
enabled = false

[browser]
enabled = false
```

## Keybinds (`[keys]`)

Bind a key with `[keys] <action> = "prefix+x"` (the prefix is `ctrl+b`), or several with `<action> = ["cmd+[", "alt+,"]`. Setting an action replaces all of its default keys; `""` unbinds it.

| Action | Default | What it does |
|---|---|---|
| `settings` | `prefix+s` | Opens the settings panel (upstream) |
| `reload_config` | `prefix+shift+r` | Reloads `config.toml` (upstream) |
| `open_notification_target` | `prefix+o` | Focuses the tab of the visible notification (upstream) |
| `clear_pane` | unbound | Clears the focused pane (upstream) |
| `sidebar_search` | `prefix+f` | Focuses or leaves the tabs sidebar search field |
| `tab_history_back` | `cmd+[`, `alt+,` | Previously focused tab, browser-style |
| `tab_history_forward` | `cmd+]`, `alt+.` | Forward again after going back |
| `toggle_info_pane` | unbound | Shows or hides the focused tab's notes pane |
| `toggle_browser_dock` | `prefix+shift+b` | Docks the companion browser on the right of the focused tab, or undocks it |
| `toggle_browser_view` | `prefix+shift+z` | The focused tab's Browser view: the companion browser over the whole tab |
| `focus_browser` | unbound | Gives the companion browser the keyboard |
| `toggle_agent_suspend` | unbound | Suspends the focused agent, or activates it again |
| `handoff_compact` | unbound | The focused Claude agent writes a handoff, then `/compact` |
| `restart_agent` | unbound | Exits the focused idle agent and relaunches it with its resume command |
| `toggle_tab_important` | unbound | Marks the focused tab important or unmarks it (old name `toggle_tab_remind` accepted) |
| `cycle_tab_color` | unbound | Cycles the tab color: none, red, orange, yellow, green, cyan, blue, purple |
| `move_tab_to_group` | unbound | Tabs layout: moves the focused tab to a group by name |
| `toggle_groups_folded` | unbound | Tabs layout: folds every group, or expands all |
| `open_coordinator` | unbound | Focuses the coordinator tab, creating it when gone |
| `open_browser` | unbound | Opens the Browser overlay |
| `open_news` | unbound | Focuses the News tab |

`cmd+[` / `cmd+]` only work if your terminal passes Cmd through to herdr; most macOS terminals keep Cmd for themselves. That is why `alt+,` / `alt+.` are bound too: they reach herdr in any terminal that sends Option as Alt (Ghostty: `macos-option-as-alt = true`; iTerm2 and Terminal.app have the same setting). herdr takes them before the pane, so your shell no longer sees `alt+.` (insert last argument). The toolbar's `‹` `›` buttons always work. Ghostty passes Cmd on with `keybind = super+[=csi:91;9u` and `keybind = super+]=csi:93;9u`.

### Suggested key set

Everything else above stays unbound until you bind it. This is one full set, all on direct keys; copy what you like into `[keys]`:

```toml
[keys]
toggle_info_pane = "alt+n"
previous_tab = "alt+a"
next_tab = "alt+d"
move_tab_previous = "alt+i"
move_tab_next = "alt+o"
new_tab = "alt+t"
rename_tab = "alt+r"
toggle_agent_suspend = "alt+s"
switch_tab = "alt+1..9"
move_tab_to_group = "alt+g"
toggle_groups_folded = "alt+e"
cycle_tab_color = "alt+c"
open_notification_target = "ctrl+space"
tab_history_back = ["cmd+[", "ctrl+shift+space"]
```

`previous_tab`, `next_tab`, `new_tab`, `rename_tab`, `switch_tab` and `open_notification_target` replace their `prefix+...` defaults; add those back in a list to keep both, e.g. `next_tab = ["alt+d", "prefix+n"]`. `tab_history_back` here drops the default `alt+,`; `tab_history_forward` keeps its defaults. Alt-letter keys only reach herdr when the terminal sends Option as Alt, and herdr takes them before the pane, so they shadow your shell's word keys (`alt+d` deletes a word, `alt+t` swaps words, `alt+c` capitalizes one). Change any of them to keys you do not use there.

## Not configurable

These agent limits are built in (`src/agents_model/limits.rs`) and have no config keys:

- 200 delivered messages per sender per hour
- Loop guard: more than 30 messages between one pair (both directions) in 10 minutes
- 20 agent-opened tabs or reopens per caller per hour, outside a user turn
- 300 soft edits (rename, meta, notes, cosmetic) per caller per hour
- 2 s gap between messages from one sender to one target (a reply is exempt)
- 5 min gap between urgent messages from one sender to one target
- 40 live agent-opened panes per team
