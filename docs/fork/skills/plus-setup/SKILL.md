---
name: plus:setup
description: "Guided herdr+ onboarding and check-up: a short tour (shows the showcase and configuration pages), the opt-in options, the installs (Claude/Codex hooks, plus:* skills, companion browser, shell hook), a health check and a safe live-handoff restart. Safe to re-run: it checks first and offers only what is missing. Use only when the user asks: '/plus:setup', 'set up herdr+', 'onboard me', 'configure herdr+', 'check my herdr+ setup'. Never on your own."
user-invocable: true
---

# plus:setup: onboarding and check-up

For a user who built and installed herdr+ per `docs/fork/SETUP.md` and runs this in a herdr+ Claude tab.
Every run starts from the current state, so it doubles as a later "check / customize" pass.

## Rules
- **Consent.** Explain each option in one or two plain lines, ask with `AskUserQuestion` (choices, incl. "skip"),
  act only after a yes. Never write anything without one.
- **Backup before every write**, same run, dated: `cp -p "$f" "$f.bak-$(date +%Y%m%d-%H%M%S)"` for each file the
  step touches that exists. Which files: see the table in step 4. Say the backup path in your reply.
- **Idempotent.** A step already done gets one line ("done: claude hooks current (v17)") and no question.
- Edit `config.toml` inside its existing tables (a second `[agents]` table is a TOML error). Then
  `herdr config check`, and after a yes `herdr server reload-config` (same as `prefix+shift+r`).
  The user can also flip most options themselves in Settings (`prefix+s`); offer that.

## 1. Preconditions and state (read-only, every run)
1. `command -v herdr`, `[ -n "$HERDR_PANE_ID" ]` and `herdr coordinator --help` (upstream herdr has no
   `coordinator`). If any fails: say herdr+ isn't installed or this isn't a herdr+ pane, point to
   `docs/fork/SETUP.md` (sections 2-4), stop.
2. Find the checkout (for the pages and the skills script): the cwd if it has `docs/fork/herdr-plus.html`,
   else `~/src/herdr-plus` (SETUP's default) if it has it, else ask the user for the path.
   (`herdr --version` prints only the version, not the path.)
3. Gather in one go, quietly:
   ```bash
   herdr integration status | grep -E '^(claude|codex):'
   "$REPO/scripts/install-plus-skills.sh" --dry-run
   herdr browser doctor
   herdr config check
   herdr coordinator status
   cat "${XDG_CONFIG_HOME:-$HOME/.config}/herdr/config.toml"
   ```
   Read the doctor by its line names (`companion_app`, `helper`, `mcp_claude`, `mcp_codex`, `shell_hook`, the
   `info [agents] wrap = …` line), not by a count. Show a short done / missing / different list, then go on.

## 2. Tour (offer; skip if the user has seen it)
Three lines on what herdr+ adds: agents that see and message each other (`herdr_agents` tools), team groups,
notes pane, a browser agents and the user share, the coordinator, suspend/autosuspend and auto handoff.
Then show the pages, one call each (relative links may not work in the sandboxed tab):
`browser_show file=<REPO>/docs/fork/herdr-plus.html focus=true`, then the same for `config.html`.
No herdr browser yet: `open <file>` (the browser-steering hook may deny it once; the same call again runs).

## 3. Options (one `AskUserQuestion` per group; skip groups already set as the user wants)
| Group | Keys (default) | Plain words |
|---|---|---|
| Agents | `[agents] wrap`, `tools`, `instructions` (off) | Recommended all on: plain `claude`/`codex` in herdr panes get the agents tools and herdr+ instructions. |
| Coordinator | `[coordinator] enabled` (off), `model` | A Claude in a pinned tab that watches agents and suggests next steps; costs Claude turns (`cap_hour` 12, `cap_day` 80). Use `herdr coordinator enable` (writes the key itself). |
| Autosuspend | `[agents.autosuspend] after` (`"off"`) | Suspends seen, idle Claude agents after e.g. `"1d"`; a message wakes them. |
| Browser | `[browser] enabled` (on), `window` (off) | The companion browser is the default; the separate Chromium window only if they want it. |
| Notifications | `[ui.toast] delivery` (`"off"`), `[ui.toast.herdr] sticky` (off), `[ui.sound] enabled` (on) | Toasts in herdr (`"herdr"`), the terminal or the system; sounds for done / needs input. |
| Keys | the "Suggested key set" in CONFIG.md | Optional alt-key set; warn alt keys shadow shell word keys and need Option-as-Alt. |
| Tab history | `cmd+[` / `cmd+]`, `alt+,` / `alt+.` (bound) | Already on; Cmd needs the terminal to pass it (Ghostty `keybind = super+[=csi:91;9u`, `super+]=csi:93;9u`). |

Fork defaults to mention once, no question: the `tabs` sidebar is the default (`sidebar_layout = "spaces"` goes
back); `[update] version_check` / `manifest_check` are off and must stay off: **`herdr update` replaces herdr+
with upstream herdr**. Update herdr+ only from source (SETUP section 9). If config sets either to `true`, offer to remove it.

## 4. Installs (only what step 1 found missing or outdated)
| Step | Command | Backs up |
|---|---|---|
| Claude hooks | `herdr integration install claude` | `~/.claude/settings.json` |
| Codex hooks (if `codex` is installed) | `herdr integration install codex` | `~/.codex/config.toml`, `~/.codex/hooks.json` |
| plus:* skills | `"$REPO/scripts/install-plus-skills.sh"` (backs up differing copies itself) | - |
| Companion browser | `herdr browser setup --companion --yes` (asks about a ~115 MB Electron download: get the yes first) | - |
| Sidecar + MCP | `herdr browser setup` (npm ci; registers `herdr-browser` for Claude and Codex) | `~/.claude.json`, `~/.codex/config.toml` |
| zsh shell hook | `herdr browser setup --shell` (zsh only; plain `claude`/`codex` go through the wrap) | `~/.zshrc` |
| Chromium window (only if wanted) | SETUP section 7: `herdr browser install-chromium <Chromium.app>`, then `[browser] window = true` | `config.toml` |

## 5. Health check and restart
1. Re-run `herdr integration status --outdated-only`, `herdr browser doctor`, `herdr config check`. Summarize in
   at most 5 lines: ok, still missing, and the fix for each failure (SETUP section 10 has the table).
2. Offer a restart only if something changed that the server must load (new binary, coordinator, browser).
   Say plainly: `herdr server live-handoff` moves every pane to a fresh server; shells and agents keep running and
   nothing is lost. It disconnects the TUI: when it prints "live handoff complete", run `herdr` again to reattach.
   Running agents pick up new hooks, MCP tools, instructions and skills only when relaunched (exit and start
   `claude` again, or the tab menu's "Restart agent"); this session too.
3. First list working agents (other than this pane):
   `herdr agent list | jq -r '.result.agents[] | select(.agent_status=="working") | [(.name // "-"), .pane_id] | @tsv'`.
   If any, say so. Then ask for an explicit go **right before** running it. No go, no restart.

## Never
- Run `herdr update`, `herdr update --handoff` or `herdr channel set …`.
- Restart, stop or hand off the server without a go given just now (`herdr server stop` ends every pane).
- Edit files in the checkout, `~/.config/herdr/coordinator`, or the hook scripts herdr writes; change config only
  through the steps above.
- Copy anyone else's config (`~/.config/herdr`, `~/.claude`, `~/.codex`), or add the owner-only bits in SETUP's
  appendix (`[news]`, `[agents.handoff] command`, `ui.tab_bar_right`, `claude-z`).
