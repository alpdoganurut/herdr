# herdr+ companion browser

An Electron browser window that herdr drives as its browser host: one group of pages per herdr tab,
the window follows the focused herdr tab, and agents' `herdr browser …` / MCP ops act on the same
views over CDP. Contract: `.local/prd/companion/contract.md` (not in git).

## Installed and run by herdr

```
herdr browser setup --companion        # asks before downloading Electron 45.2.1 (~115 MB); --yes answers yes
```

installs this app under herdr's browser home (`<state dir>/browser/companion-app/`: the app files of
this herdr, Electron, and on macOS a "herdr+ companion" app with the eye-plus icon). herdr then runs
it: the first page an agent opens starts it in the background (`[browser] companion_autostart = true`
starts it with the server), a crash restarts it (after 1, 2, 4, 8 s; it gives up after 5 crashes in
5 minutes), it quits with the server and survives a live handoff. `herdr browser companion
status|start|stop|restart|focus|log`, the Browser row's menu and the Browser overlay control it.
A rerun of setup only updates what changed; after a herdr update the server refreshes the app files
(a restart applies them: the bar shows "Update ready · Restart").

Managed mode (what herdr passes): `--managed --launch-token <hex>` (the registration carries the token,
so herdr knows the lease is its process), `--background` (never shown, never activated: an agent's
start), `HERDR_COMPANION_MANAGED=1`. Managed, it quits when SIGTERM'd or when herdr's socket stays
unreachable for 30 s, and its control page's `quit()` / `raise({user})` serve herdr's stop and focus.
`HERDR_COMPANION_TEST_HOOKS=1` adds `__crash()` / `__hang()` to the control page (tests only).

## Try it (one command)

```
scripts/try-launcher.sh [url]
```

Run in a plain Ghostty window (not inside herdr, tmux or ssh). It builds what is missing (herdr in
release mode, `node_modules`, Electron), installs the companion into a throwaway state dir with
`herdr browser setup --companion` (Electron cloned from `node_modules`, no download) and opens a
throwaway herdr in that window (temp HOME/XDG dirs, its own socket; your live herdr is never
touched). Its first pane lists what to try: an agent's `herdr browser open` (the on-demand start),
`ctrl+b B` or `◨` (the dock), `◎ Browser` or `ctrl+b Z` (Browser view), the Browser row's menu,
`companion-crash` (a crash restart through the test hook) and the checks to do by hand (move,
resize, Spaces, full screen, the keyboard hand-off). `url` is opened agent-style in the first pane.
Leaving herdr (ctrl+b q) stops the server, the companion goes with it, and the temp folder is
deleted; after a live handoff the script attaches the UI again. `TRY_HEADLESS=1` runs a hidden
self-check instead (the on-demand start, a crash restart; the exit status says how it went).

`scripts/try.sh [url]` is the older variant: the companion started by hand next to a throwaway
herdr with the herdr+ window on (`host = "auto"`).

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

## Docked inside herdr's window

herdr's TUI can keep an area of its terminal window for the browser: the dock on the right of a
tab (`prefix+⇧B`) or the whole tab (Browser view, `prefix+⇧Z`). The client publishes it as a
placement (`browser.placement.set`: the herdr tab, the cells, its terminal's pixel extent and cell
size, its process ancestry); the companion long-polls every placement (`browser.placements`), covers
the target (the foreground client's) and reports what it does (`browser.placement_report`:
covering, hidden, moving, unbound, unsupported, elsewhere), which herdr's placeholder shows.

* `src/dock/placements.js` — the long poll, the reports (one in flight per placement, the latest
  change after it: each request has its own connection, so two could land out of order), the
  target choice.
* `src/dock/window-probe.js` + `probe.jxa.js` — the window source: a persistent `osascript`
  reading CGWindowList through the JXA ObjC bridge (bounds, owner pid, layer, on-screen; no
  Accessibility, no Screen Recording), 10 Hz for the bound window and the windows above it (20 Hz
  while it moves), the client's candidate windows every 5th sample. About 3 % of a core while docked;
  nothing while not docked. `HERDR_COMPANION_FAKE_WINDOWS=<json>` (test hooks only) replaces it.
* `src/dock/geometry.js` + `ghostty-config.js` — where the area is on screen, from the window frame,
  the display scale, the pty's pixel extent and Ghostty's padding / balance / titlebar settings (its
  config files and its own command line, read again when the placement's pixel extent or cell size
  changes, at most once a second: a reload that changes the padding); an area that reaches the
  grid's edge runs to the window's edge. A window that does not match the client's grid (a Ghostty
  split) is refused with the fix.
* `src/dock/dock.js` — the core (bind the placement to its terminal window by the client's
  ancestry; with several windows the frontmost when the client's focus flips or the user clicks the
  placeholder; hide while the window moves and show again 120 ms after it settles; hide off screen)
  and the Electron side: a second, frameless window over the area, ordered directly above the
  terminal window (`moveAbove`, again on its `blur` and whenever another window gets between), the
  pages re-parented into it, the main window hidden while docked, accessory activation policy (out of
  ⌘-Tab and the Dock; needed to join a full-screen Space, where `visibleOnFullScreen` is set). The
  docked bar is always Dusk, without traffic lights or a drag region. ⌘⎋ gives the keyboard back to
  the terminal; ⌘T/W/L/R/[/] and ⌘⇧S work without the menu bar. Docked, a focus on another herdr
  tab's page keeps the dock on its tab: an agent's selects and marks the page there, the user's own
  focuses that herdr tab (the dock follows where that tab has it).
* The bar's count pill (`3 · 12`: pages in this session · in every session; `src/info.js`) lists
  every session with pages and focuses the herdr tab you pick; the window title carries the count.

`test/e2e-placements.sh` runs the whole path headless against a real isolated herdr (a TUI client in
a pty presses prefix+⇧B and prefix+⇧Z; a fake window plays Ghostty);
`scripts/smoke_companion_dock.sh` measures it live over background Ghostty instances.

## Develop

```sh
npm test                 # unit tests (groups/sync logic, line parser, socket path, managed mode, dock geometry/core, info), no Electron
npm run e2e              # headless Electron against test/mock-herdr.mjs (headless; never shows a window)
test/e2e-real-herdr.sh   # headless Electron against a REAL isolated herdr (target/release/herdr or $HERDR_BIN):
                         # temp HOME/XDG dirs, no inherited HERDR_* vars, no Chromium window (fallback asserted by routing)
test/e2e-placements.sh   # the dock against a REAL isolated herdr: a TUI client in a pty docks the browser,
                         # the companion (headless, fake windows) covers it where the geometry model puts it
test/e2e-launcher.sh     # herdr's launcher against a REAL isolated herdr: setup --companion (cloning this
                         # node_modules/electron/dist, no download), on-demand start, crash backoff and give-up,
                         # restart, clean stop, live handoff survival, the stop with the server (headless)
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
printf '[browser]\nenabled = true\nwindow = true\nhost = "auto"\n' > $T/config/herdr/config.toml
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
| `src/managed.js` | managed mode: the launcher's flags, register fields, the startup window rule, quit signals, the socket watchdog |
| `src/host.js` | herdr link: register, heartbeat, events stream, reconnect |
| `src/herdr-api.js` | newline-JSON socket client, socket path resolution |
| `src/groups.js` | herdr tab → group bookkeeping, host_state merge, host_sync payloads (pure) |
| `src/views.js` | WebContentsView per page, partitions, html sandbox, CDP target id, re-parenting (docked) |
| `src/dock/` | docked mode: placements feed, window probe (JXA), geometry model, Ghostty config, the dock core and its Electron side |
| `src/info.js` | the count pill and the sessions menu (pure) |
| `src/preload/control.js`, `src/preload/strip.js` | context bridges |
| `src/ui/` | strip page (bar variant A, light/dark follows the system) and control page; `icons/` = icon set A masters |
| `assets/icon/` | eye-plus app icon: PNGs (window/dock), `eye-plus.icns` for packaging (`make-icns.sh` rebuilds it) |
| `test/` | unit tests, mock herdr, e2e |
| `install/` | the Electron runtime's npm manifest and pinned lock (`herdr browser setup --companion` runs `npm ci` with it) |
| `spike/` | P3.0 spike |
