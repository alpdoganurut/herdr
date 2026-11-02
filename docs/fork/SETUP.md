# herdr+ setup on macOS (Apple Silicon)

What you get: [herdr-plus.html](herdr-plus.html). Configuration: [CONFIG.md](CONFIG.md) ([config.html](config.html)).

herdr is a terminal agent runtime: one TUI that runs Claude Code, Codex and shells in
workspaces, tabs and panes, and tracks each agent's state. herdr+ is a fork that adds
agent-to-agent tools (`herdr_agents` MCP), team rules, a herdr-owned Chromium browser
for agents, and a coordinator agent with a dashboard.

**Included:** the herdr+ binary, Claude Code and Codex integrations (hooks, agents tools,
team rules), the `plus:*` Claude skills, the herdr browser, the coordinator.

**Not included:** Ghostty config (use any terminal), and the News digest. News is off by
default; keep it off by leaving `[news]` out of your config (or `[news]` `enabled = false`)
and never running `herdr news run`.

Never copy files from someone else's `~/.config/herdr`, `~/.claude` or `~/.codex`. Every
setup step below writes your own.

---

## 1. Prerequisites

Run these in a normal terminal (zsh, the macOS default).

1. Xcode Command Line Tools (clang, linker, `git`, `codesign`, `/usr/bin/python3`):

   ```bash
   xcode-select --install
   xcode-select -p && python3 --version
   ```

   `python3` is required at runtime: the Claude and Codex hooks exit silently without it,
   so you get no agent state and no error.

2. Homebrew (skip if `brew --version` works):

   ```bash
   /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
   ```

3. Rust through rustup. The repo pins 1.96.1 in `rust-toolchain.toml`; rustup fetches it on
   the first build. A Homebrew `rust` ignores the pin, so remove it if present.

   ```bash
   brew list rust >/dev/null 2>&1 && brew uninstall rust
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
   source "$HOME/.cargo/env"
   ```

4. Zig **exactly 0.16.0** (builds the vendored libghostty-vt). Other versions fail.
   Check the exact file name on https://ziglang.org/download/ under 0.16.0, aarch64-macos.

   ```bash
   mkdir -p ~/.local/zig && cd ~/.local/zig
   curl -LO https://ziglang.org/download/0.16.0/zig-aarch64-macos-0.16.0.tar.xz
   tar -xf zig-aarch64-macos-0.16.0.tar.xz
   echo 'export ZIG="$HOME/.local/zig/zig-aarch64-macos-0.16.0/zig"' >> ~/.zshrc
   source ~/.zshrc && "$ZIG" version    # must print 0.16.0
   ```

   (`brew install zig` also works while `zig version` prints 0.16.0, but Homebrew will move on.)

5. Agent CLIs: Claude Code (required for the coordinator) and optionally Codex. Install
   each, run it once and sign in.

   ```bash
   npm install -g @anthropic-ai/claude-code   # or Anthropic's native installer
   claude --version
   brew install codex                          # optional; or npm install -g @openai/codex
   codex --version
   ```

6. Browser prerequisites (only if you want the herdr browser): Node.js 20+ with npm, and a
   Chromium.app (section 7).

   ```bash
   brew install node
   node --version    # v20 or newer
   ```

7. Small tools: `jq` (used by `/plus:status`; macOS 15+ ships `/usr/bin/jq`) and,
   optionally, `terminal-notifier` for nicer desktop notifications.

   ```bash
   command -v jq || brew install jq
   brew install terminal-notifier   # optional
   ```

8. Only if you will hack on herdr+ itself: `brew install just bun && cargo install cargo-nextest --locked`,
   then `just test`. Not needed to build or run.

---

## 2. Clone and build

The fork's GitHub default branch is `master`, which is plain upstream herdr. herdr+ lives on
branch **`fork`**. Always clone with `-b fork`.

```bash
mkdir -p ~/src && cd ~/src
git clone -b fork https://github.com/alpdoganurut/herdr.git herdr-plus
cd herdr-plus
git branch --show-current          # fork
cargo build --release --locked
```

- The first build needs open network access to crates.io, deps.files.ghostty.org and
  codeberg.org (Zig fetches libghostty-vt deps into `~/.cache/zig`).
- Every build prints a `cargo:warning` telling AI agents about the external-contributor
  policy. It is expected for a private build; you are not opening issues or PRs.
- Always build `--release`. Debug builds (`cargo build`, `cargo run`) use
  `~/.config/herdr-dev` and a separate server, so nothing you configure with them applies.

---

## 3. Install

Install to a fixed path **before** running any setup command. The coordinator's MCP config,
the browser MCP registrations and the agent wrap all record the absolute path of the herdr
binary that ran them. Running them from `target/release` ties them to the build directory.

Use copy, sign, then rename. Never `cp` straight over a running `~/.local/bin/herdr`: that
rewrites the file in place, invalidates its signature, and macOS SIGKILLs the next launch.

```bash
cd ~/src/herdr-plus
mkdir -p ~/.local/bin
cp target/release/herdr ~/.local/bin/herdr.new
codesign -s - -f ~/.local/bin/herdr.new
[ -e ~/.local/bin/herdr ] && cp -p ~/.local/bin/herdr ~/.local/bin/herdr.prev
mv -f ~/.local/bin/herdr.new ~/.local/bin/herdr
```

Put `~/.local/bin` first on PATH and check:

```bash
grep -q 'HOME/.local/bin' ~/.zshrc || echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
source ~/.zshrc
which -a herdr        # ~/.local/bin/herdr must be first
herdr --version       # herdr 0.9.5
```

### If you already have upstream herdr

```bash
which -a herdr
brew list herdr >/dev/null 2>&1 && brew uninstall herdr
cp -R ~/.config/herdr ~/.config/herdr.bak-upstream 2>/dev/null
```

Stop a running upstream server with the **old** binary (from a terminal outside herdr):
`herdr server stop`. This ends its panes. Then install herdr+ as above.

### Warning: upstream's updater replaces herdr+

`herdr update`, `herdr update --handoff` and `herdr channel set ...` are upstream's
self-updater. They download upstream herdr and replace `~/.local/bin/herdr`, which drops the
coordinator, agents tools and browser. herdr+ reports the same version as upstream (0.9.5),
so once upstream ships a newer release herdr will offer an update. **Never run those
commands**, and turn the checks off in config (section 4). Update herdr+ only as in section 9.

---

## 4. Minimal config and first run

Write a fresh, minimal config. Do not start from `herdr --default-config` and append: its
output already contains an `[update]` table, and a second one is a TOML error.

```bash
mkdir -p ~/.config/herdr
[ -e ~/.config/herdr/config.toml ] && echo "config.toml exists: edit it by hand" || cat > ~/.config/herdr/config.toml <<'EOF'
[update]
version_check = false     # no upstream update notices
manifest_check = false    # keep the fork's bundled agent-detection rules

[agents]
wrap = true               # route claude/codex in herdr panes through `herdr agent wrap`
tools = true              # give agents the herdr_agents MCP tools
instructions = true       # add the herdr+ instruction paragraph

[coordinator]
enabled = true
# model = "opus"          # optional; unset = Claude Code's default model
EOF
```

If the file already exists, add or edit those keys inside the existing tables instead.

Leave out: `[news]`, `[agents.handoff] command` (empty uses herdr's own handoff prompt),
`ui.tab_bar_right`, and `[agents] context_window` unless your model really has that window.

First run:

```bash
herdr
```

This starts the server and attaches the TUI. After config edits while it runs:
`herdr server reload-config`.

---

## 5. Claude Code and Codex integrations

Run from the installed binary (it is first on PATH now):

```bash
herdr integration install claude
herdr integration install codex       # skip if you do not use Codex
herdr integration status
herdr integration status --outdated-only   # should list nothing
```

This writes hook scripts under `~/.claude/hooks` and `~/.codex` and registers them in each
agent's settings. Reinstall after each herdr+ update when `--outdated-only` lists them.

Install the zsh shell hook so a plain `claude` or `codex` typed in a herdr pane goes through
the wrap (agents tools, instructions, team roster, `codex --no-daemon`):

```bash
herdr browser setup --shell
grep 'herdr+' ~/.zshrc
```

It adds one guarded line to `~/.zshrc` (or `$ZDOTDIR/.zshrc`) that sources
`~/.config/herdr/shell/herdr-plus.zsh`. It is zsh-only; bash and fish get no wrap. Open a
new pane afterwards.

Verify inside a new herdr pane:

```bash
herdr agent wrap claude --print | grep herdr_agents    # the agents MCP is on the command line
herdr agent wrap codex --print | grep herdr_agents
```

Then start `claude` in that pane and run `/mcp`: `herdr_agents` should be listed. Ask it to
call `agents_whoami`.

**Relaunch running agents.** Agents started before the install or before `wrap`/`tools` were
on do not pick anything up live. Exit each `claude`/`codex` and start it again in its pane.

Per launch you can bypass the wrap with `claude --no-herdr`, `HERDR_NO_WRAP=1 claude`, or
`command claude`.

---

## 6. The plus:* Claude skills

From the clone:

```bash
cd ~/src/herdr-plus
scripts/install-plus-skills.sh --dry-run
scripts/install-plus-skills.sh
```

This installs into `${CLAUDE_CONFIG_DIR:-~/.claude}/skills/plus:<name>`. It is safe to
re-run; a differing existing copy is moved to `~/.claude/skills-backup/` first. Restart
Claude Code afterwards.

| Skill | What it does |
|---|---|
| `/plus:notes` | Writes or refreshes this session's notes in the notes pane (context, work in flight, plan). Only on request. |
| `/plus:notify` | Sets, shows or clears when this agent should alert you through `agents_notify`. |
| `/plus:status` | One-shot digest of agents: working, blocked, done, what waits on you. Args: `[group \| all]`. |
| `/plus:team-build` | Runs a multi-agent build as a herdr+ team: design, contracts, lanes, review, gate, cutover. |

Dependencies: they only work in an agent launched inside herdr+ with the `herdr_agents`
tools (section 5). `/plus:status all` needs `jq` and herdr+'s `herdr` on PATH.
`/plus:team-build` works best with Opus access; Codex is optional for its cross-model audit.

---

## 7. The herdr browser

A Chromium window herdr owns, which your agents drive through the `herdr-browser` MCP
server and which you can use too.

1. **Get a Chromium.app for Mac arm64.** The repo ships none. Use Chromium, not Google
   Chrome: auto-detection never looks for Chrome, the bundle must contain
   `Contents/MacOS/Chromium`, and Chrome ignores `--load-extension`. One source is the
   Mac_Arm snapshots at
   https://commondatastorage.googleapis.com/chromium-browser-snapshots/index.html?prefix=Mac_Arm/
   (download `chrome-mac.zip`). Unzip it into its own new folder.

2. **Install a branded, signed copy** (copies to `~/Applications/herdr+ Browser.app`,
   strips quarantine, ad-hoc signs, refreshes LaunchServices):

   ```bash
   herdr browser install-chromium ~/Downloads/chrome-mac/Chromium.app
   ```

   Lookup order with the default `executable = "auto"`: `~/Applications/herdr+ Browser.app`,
   `~/Applications/Chromium.app`, `/Applications/Chromium.app`. For another location set
   `[browser]` `executable = "/path/to/Chromium.app"`. If you skip `install-chromium`, run
   `xattr -dr com.apple.quarantine /path/to/Chromium.app` once.

3. **Set up the sidecar and MCP registrations** (runs `npm ci` for playwright-core, needs
   npm registry access; registers `herdr-browser` for Claude Code at user scope via
   `claude mcp add-json` and for Codex in `~/.codex/config.toml`):

   ```bash
   herdr browser setup
   ```

   Variants: `--claude` or `--codex` for one agent only, `--no-mcp` to skip registrations,
   `--node /abs/path/to/node` if node is somewhere unusual (or `[browser]` `node = "..."`).
   `claude` must be on PATH for the Claude registration.

4. **Check:**

   ```bash
   herdr browser doctor
   claude mcp list | grep herdr-browser
   grep herdr-browser ~/.codex/config.toml
   ```

   All six doctor checks should be ok: `executable`, `helper`, `extension`, `mcp_claude`,
   `mcp_codex`, `launch_context`.

5. **First launch:** `herdr browser start` (or let an agent call `browser_open`). macOS asks
   for Keychain access to "Chromium Safe Storage": choose Always Allow. It asks again after a
   re-install because the ad-hoc signature changes. No Screen Recording or Accessibility
   permission is needed.

6. If a Codex app/daemon was already running, restart it so it loads
   `[mcp_servers.herdr-browser]`. Relaunch running Claude/Codex agents too.

Optional: `[browser]` `autostart = true` launches the `main` profile with the server
(default: lazy, on first use). Profiles and logins live under `~/.local/state/herdr/browser/`;
sign in by hand in the browser window. Re-run `herdr browser setup` if you ever move the
herdr binary.

---

## 8. The coordinator

A Claude Code session the herdr server runs in a pinned `coordinator` tab. It watches your
agents, gets woken by their status changes and messages, and keeps a dashboard and board.
It has no built-in scheduled jobs.

It needs: `claude` on the PATH of a new zsh login shell and signed in, herdr+ installed at
`~/.local/bin/herdr`, and `[coordinator] enabled = true`. It does not use Codex and does not
need News.

1. Check Claude from a login shell:

   ```bash
   zsh -lc 'command -v claude && claude --version'
   ```

2. Enable it. Either the config key from section 4 (then restart the server so it runs from
   the installed binary), or while the server runs:

   ```bash
   herdr coordinator enable
   ```

   Settings -> Coordinator writes the same key.

3. Open the pinned `coordinator` tab. On first launch Claude asks whether to trust the
   folder `~/.config/herdr/coordinator`. Answer it once; until then herdr shows "coordinator
   needs you". At least one workspace must exist.

4. Check:

   ```bash
   herdr coordinator status
   herdr coordinator dashboard          # opens it
   herdr coordinator dashboard --print  # prints the URL (http://127.0.0.1:7718)
   ```

   If port 7718 is taken (`lsof -iTCP:7718 -sTCP:LISTEN`), set `[coordinator]`
   `dashboard_port = 7728`; `0` turns serving off.

5. Optional: `herdr browser setup --claude` gives the coordinator the browser tools it uses
   to show the dashboard. Without it the coordinator still works.

Other verbs: `herdr coordinator start [--new]` (restart, `--new` = fresh session),
`herdr coordinator wake`, `herdr coordinator disable`.

### Keeping wakes within limits

All in `[coordinator]`; the values shown are the defaults.

```toml
[coordinator]
enabled = true
cap_hour = 12             # wake-ups per hour
cap_day = 80              # wake-ups per day
periodic_minutes = 60     # slow periodic check while changes are pending
relaunch_cap_hour = 3     # relaunches of a missing coordinator per hour before "down"
notify = true             # coordinator notifications
notify_daily_cap = 6      # suggestion notifications per day
quiet_hours = ""          # e.g. "22:00-08:00": notifications wait (a "down" alert does not)
wake_scope = "opened"     # opened | teams | all: which agents wake it
```

Lower `cap_hour`/`cap_day` and keep `wake_scope = "opened"` to spend fewer Claude turns.

State lives in `~/.config/herdr/coordinator` (override with `HERDR_COORDINATOR_DIR`). herdr
seeds it; do not copy anyone else's.

---

## 9. Updating later

```bash
cd ~/src/herdr-plus
git pull --ff-only origin fork
cargo build --release --locked
cp target/release/herdr ~/.local/bin/herdr.new
codesign -s - -f ~/.local/bin/herdr.new
cp -p ~/.local/bin/herdr ~/.local/bin/herdr.prev
mv -f ~/.local/bin/herdr.new ~/.local/bin/herdr
herdr --version
```

The running server keeps the old binary. Move it to the new one without losing panes:

```bash
herdr server live-handoff
```

A new server takes over the live panes; shells and agents keep running. When it prints
"live handoff complete", every client was disconnected: run `herdr` again to reattach. Agent
status can show idle briefly until the next hook report. It works across a protocol change
too. If it fails, finish your work, then `herdr server stop` (this ends pane processes) and
start `herdr` again.

Then:

```bash
herdr integration status --outdated-only    # reinstall any listed: herdr integration install <agent>
scripts/install-plus-skills.sh              # refresh the skills
herdr browser doctor                        # re-run `herdr browser setup` if a check fails
```

Relaunch agents to pick up new tools or instructions. Never use `herdr update`.

---

## 10. Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| Build panics `zig executable not found` or `zig build ... failed` | Zig missing or not 0.16.0 | Section 1 step 4; `"$ZIG" version` must print 0.16.0 |
| `cargo --version` is not 1.96.1 in the clone | Homebrew rust shadows rustup | `brew uninstall rust`; `source ~/.cargo/env` |
| Build fails fetching deps | Network blocks crates.io / deps.files.ghostty.org / codeberg.org | Build once on an open network |
| herdr is upstream (no coordinator, no `herdr coordinator`) | Cloned `master`, or another herdr earlier on PATH | `git checkout fork`; `which -a herdr`; `brew uninstall herdr` |
| herdr is killed at launch (Code Signature Invalid) | Binary overwritten in place | Use the `.new` + `codesign` + `mv -f` recipe |
| "Stop the old server to use the new version" | Running server is upstream or an older protocol | `herdr server live-handoff`, else `herdr server stop` and `herdr` |
| Update notice from upstream; herdr+ features gone after `herdr update` | Upstream updater | `[update] version_check = false`, `manifest_check = false`; reinstall from source |
| Agents show no state, no handoff reports | `python3` missing or hooks not installed | `python3 --version`; `herdr integration install claude` / `codex` |
| `herdr integration status --outdated-only` lists agents | Hooks older than this build | `herdr integration install <agent>` |
| No `herdr_agents` tools in an agent | Wrap off, shell hook missing, non-zsh shell, or agent started earlier | `[agents] wrap/tools = true`; `herdr browser setup --shell`; new pane; relaunch agent |
| Config or browser setup "does nothing" | Done with a debug build (`~/.config/herdr-dev`) | Use the release binary in `~/.local/bin` |
| MCP entries point into `target/release` | Setup ran from the build dir | Re-run `herdr browser setup` from `~/.local/bin/herdr`; restart server |
| Doctor `executable` fails | No Chromium.app found | Section 7 steps 1-2, or `[browser] executable = "..."` |
| Doctor `helper` fails | Node < 20, npm missing, or npm registry blocked | `brew install node`; `herdr browser setup --node /abs/node` |
| Doctor `mcp_claude` fails | `claude` not on PATH during setup | Install Claude Code; `herdr browser setup --claude` |
| Doctor `mcp_codex` fails / Codex agents lack browser | Not registered, or old Codex daemon running | `herdr browser setup --codex`; restart the Codex app/daemon |
| Chromium won't open ("damaged"/blocked) | Quarantined download | `herdr browser install-chromium <app>` or `xattr -dr com.apple.quarantine <app>` |
| Repeated Keychain prompt "Chromium Safe Storage" | New ad-hoc signature | Always Allow |
| "coordinator needs you" | Claude's trust-folder prompt | Answer it in the `coordinator` tab |
| Coordinator never starts | `enabled` false, `claude` not on login-shell PATH, not signed in | `herdr coordinator enable`; `zsh -lc 'command -v claude'`; `claude` then `/login` |
| Dashboard not reachable | Port 7718 in use | `[coordinator] dashboard_port = 7728` |
| `/plus:*` skills missing or dangling in instructions | Skills not installed | `scripts/install-plus-skills.sh`; restart Claude Code |
| `/plus:status all` fails | `jq` missing | `brew install jq` |
| No desktop notifications | macOS permission | `brew install terminal-notifier`; allow in System Settings > Notifications (or for Script Editor) |
| Agent helping you build refuses, citing CONTRIBUTING.md | Upstream's external-contributor rules | Expected; a local build opens no issues or PRs. Build by hand if needed |

---

## Appendix: owner-only things to ignore

These exist in the repo or on the owner's machine and are not for you:

- `scripts/fork_sync.sh`, `scripts/fork_sync_lib.py`, the `/fork-sync` skill and `FORK.md`
  upkeep: merging upstream into `fork` and pushing.
- `~/.local/state/herdr-fork-sync/installed-sha` and staged `~/.local/bin/herdr.next`.
- The owner's remotes `mine` (SSH push) and `origin` (upstream herdrdev). Your `origin` is
  the HTTPS clone; you only pull `fork`.
- `scripts/vendor_libghostty_vt.py` and the `/home/can/...` paths (upstream maintainer).
- The Leap plugin: `/leap:*` skills (including `/leap:handoff` in the owner's
  `[agents.handoff] command`), `leap-bi` MCP, leap-games, leap-codex.
- `claude-z`: the owner's Claude wrapper. herdr prefers it when it is on PATH and uses
  `claude` otherwise; the generated `herdr-plus.zsh` defines a harmless `claude-z` function.
- `~/.config/herdr/claude-status.sh` and `ui.tab_bar_right`.
- `~/.config/herdr/si` and `~/Library/LaunchAgents/com.alp.si-*.plist`: the owner's
  scheduled jobs, not part of the coordinator.
- The owner's `~/.config/herdr/{coordinator,coordinator.json,notes,news,agents.md,plus}`,
  `~/.claude/settings.json`, `~/.claude.json` and `~/.codex/config.toml`.
- News (`[news]`, `herdr news run`).
- Ghostty config.
