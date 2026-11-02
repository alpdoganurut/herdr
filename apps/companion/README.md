# herdr+ companion browser

An Electron browser window that herdr drives as its browser host: one group of pages per herdr tab,
the window follows the focused herdr tab, and agents' `herdr browser …` / MCP ops act on the same
views over CDP. Contract: `.local/prd/companion/contract.md` (not in git).

## Try it (one command)

```
scripts/try.sh [url]
```

Run in a plain terminal (not inside herdr, tmux or ssh). It starts a throwaway herdr (temp HOME/XDG dirs,
its own socket; your live herdr is never touched) in that terminal, opens the companion window next to it,
and optionally opens `url` in the first tab. In a pane: `herdr browser open <url>`,
`herdr browser show <file.html>`; for an agent run `HOME=$REAL_HOME claude`. Leaving herdr (ctrl+b q)
quits the companion, stops the throwaway server and deletes its folder. `TRY_HEADLESS=1` runs a hidden
self-check instead (used by CI-style dry runs).

## Run

```sh
cd apps/companion
npm install
node node_modules/electron/install.js   # if node_modules/electron/dist is missing (seen here: npm skipped the postinstall binary download)
npm start -- --socket /path/to/herdr.sock
```

* `--socket <path>` (or `HERDR_SOCKET_PATH`) is required. `--default-socket` uses herdr's default
  API socket (`$XDG_CONFIG_HOME/herdr/herdr.sock`, else `~/.config/herdr/herdr.sock`; a debug herdr
  uses `herdr-dev` instead of `herdr`).
* `--profile-dir <dir>`: the Electron profile (cookies of web pages live here). Default: the app's
  user data dir (`~/Library/Application Support/herdr-companion` on macOS). One companion per profile.
* `--headless` (or `HERDR_COMPANION_HEADLESS=1`): never shows the window, hides the dock icon,
  suppresses window raises, and enables `__herdrCompanion.quit()` — for tests.

The herdr side must support `browser.host_register` (herdr+ companion lane, P2). An older herdr
answers `unknown_method` and the strip says "this herdr is too old for the companion".

## What it does

* Registers as the `companion` browser host (lease + 5 s heartbeat), re-registers with the same
  instance after a herdr restart or live handoff, unregisters on quit. herdr's API socket answers
  one request per connection, so every request opens its own; only the `events.subscribe` stream
  stays open (its close, or a failed heartbeat, means herdr went away).
* `events.subscribe` (tab/workspace events) + `browser.host_state` at startup: shows the focused
  herdr tab's pages; other sessions' pages are created lazily when shown or targeted.
* `browser.host_sync` (seq per lease, ≤ 4/s, changed groups only) keeps herdr's per-tab browser
  state current; herdr restores it across restarts.
* Strip: this session's pages (title + close), back/forward/reload/stop, URL box, send to agent
  (with note and the page selection → `browser.send_page`). ⌘T new tab, ⌘W close, ⌘L address,
  ⌘R reload, ⌘[ / ⌘] history, ⌘⇧S send. ⌘-click / `target=_blank` opens a tab in the same session.
  Double-click the session label to focus that herdr tab.
* Strip actions and page load failures/crashes go to herdr as `browser.host_report` events
  (in-page clicks and typing are never reported).
* Hidden control page `herdr-companion://control/` exposes `window.__herdrCompanion`
  (`nonce/open/select/focus/close/show/views`) for herdr's sidecar.
* HTML views (`show`): sandboxed, one in-memory partition each, `file://` limited to the file's
  directory (symlinks resolved), network blocked unless `allow_network`, all permissions denied,
  navigation outside the directory opens http(s) links as normal tabs.
* Web pages use the persistent partition `persist:herdr-companion` (own profile, no cookie import).
  Their permission requests are never granted on their own: geolocation, notifications,
  camera/microphone and clipboard read are asked in the strip (Allow once / Deny; unanswered after
  60 s or when the page leaves the origin = denied; "once" lasts until the page leaves the origin),
  sanitized clipboard writes are allowed, everything else is denied.

The DevTools port is loopback-only and unauthenticated (same exposure as the herdr+ window).

## Develop

```sh
npm test                 # unit tests (groups/sync logic, line parser, socket path), no Electron
npm run e2e              # headless Electron against test/mock-herdr.mjs (headless; never shows a window)
test/e2e-real-herdr.sh   # headless Electron against a REAL isolated herdr (target/release/herdr or $HERDR_BIN):
                         # temp HOME/XDG dirs, no inherited HERDR_* vars, no Chromium window (fallback asserted by routing)
npm run mock -- --socket /tmp/hc.sock --verify-cdp   # a mock herdr to point `npm start` at
npm run spike            # P3.0 spike: Playwright over CDP against Electron views
npx electron test/ui-shots.cjs <dir>   # bar screenshots (light/dark, fixed states) from a hidden window
```

`test/mock-herdr.mjs` implements the contract's JSON shapes plus test-only `mock.*` methods
(focus/create/close/rename tabs, restart = live handoff, control = evaluate on the control page);
like herdr it closes each connection after one answer.

Try it visibly against an isolated herdr (its own HOME, config, state and socket; nothing shared
with a running herdr). From `apps/companion`, after `cargo build --release` at the repo root:

```sh
T=$(mktemp -d /tmp/hcv-XXXXXX); H=$T/state/herdr/browser/host; mkdir -p $T/config/herdr $H
printf '[browser]\nenabled = true\nhost = "auto"\n' > $T/config/herdr/config.toml
ln -s "$PWD/node_modules" $H/node_modules     # the sidecar's playwright-core (same pin)
printf '{"version":1,"node":"%s","playwright_core":"1.63.0","assets_sha256":"","installed_at":0}\n' "$(command -v node)" > $H/runtime.json
# terminal 1: the isolated herdr TUI
env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH -u HERDR_PANE_ID HOME=$T XDG_CONFIG_HOME=$T/config XDG_STATE_HOME=$T/state ../../target/release/herdr
# terminal 2 (same T): the companion window
npm start -- --socket $T/config/herdr/herdr.sock --profile-dir $T/companion
```

Inside an isolated pane, `herdr browser open https://example.com` lands in the companion; switching
herdr tabs swaps its pages. Quit the companion and the next `open` goes to the herdr+ window instead.

## Files

| file | job |
|---|---|
| `src/main.js` | process switches, window, control page, strip IPC, focus following, sync/report writers, send page |
| `src/host.js` | herdr link: register, heartbeat, events stream, reconnect |
| `src/herdr-api.js` | newline-JSON socket client, socket path resolution |
| `src/groups.js` | herdr tab → group bookkeeping, host_state merge, host_sync payloads (pure) |
| `src/views.js` | WebContentsView per page, partitions, html sandbox, CDP target id |
| `src/preload/control.js`, `src/preload/strip.js` | context bridges |
| `src/ui/` | strip page (bar variant A, light/dark follows the system) and control page; `icons/` = icon set A masters |
| `assets/icon/` | eye-plus app icon: PNGs (window/dock), `eye-plus.icns` for packaging (`make-icns.sh` rebuilds it) |
| `test/` | unit tests, mock herdr, e2e |
| `spike/` | P3.0 spike |
