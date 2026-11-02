# herdr+ companion browser

An Electron browser window that herdr drives as its browser host: one group of pages per herdr tab,
the window follows the focused herdr tab, and agents' `herdr browser …` / MCP ops act on the same
views over CDP. Contract: `.local/prd/companion/contract.md` (not in git).

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
  instance after a herdr restart or live handoff, unregisters on quit.
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

The DevTools port is loopback-only and unauthenticated (same exposure as the herdr+ window).

## Develop

```sh
npm test                 # unit tests (groups/sync logic, line parser, socket path), no Electron
npm run e2e              # headless Electron against test/mock-herdr.mjs (headless; never shows a window)
npm run mock -- --socket /tmp/hc.sock --verify-cdp   # a mock herdr to point `npm start` at
npm run spike            # P3.0 spike: Playwright over CDP against Electron views
```

`test/mock-herdr.mjs` implements the contract's JSON shapes plus test-only `mock.*` methods
(focus/create/close/rename tabs, restart = live handoff, control = evaluate on the control page).

## Files

| file | job |
|---|---|
| `src/main.js` | process switches, window, control page, strip IPC, focus following, sync/report writers, send page |
| `src/host.js` | herdr link: register, heartbeat, events stream, reconnect |
| `src/herdr-api.js` | newline-JSON socket client, socket path resolution |
| `src/groups.js` | herdr tab → group bookkeeping, host_state merge, host_sync payloads (pure) |
| `src/views.js` | WebContentsView per page, partitions, html sandbox, CDP target id |
| `src/preload/control.js`, `src/preload/strip.js` | context bridges |
| `src/ui/` | strip page and control page |
| `test/` | unit tests, mock herdr, e2e |
| `spike/` | P3.0 spike |
