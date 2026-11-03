#!/bin/bash
# Try the companion launcher in one command: a throwaway herdr+ built from this checkout, with
# the companion browser installed and run by herdr itself (no `npm start`).
#
#   apps/companion/scripts/try-launcher.sh [url]
#
# Run it in a plain Ghostty window (not inside herdr, tmux or ssh). It builds what is missing
# (herdr in release mode, the app's node_modules and Electron), installs the companion into a
# temp state dir with `herdr browser setup --companion` (Electron cloned from this checkout's
# node_modules, no download), starts the throwaway server and opens its herdr UI in this window.
# Your live herdr, its config and its state are never touched. The first pane prints what to
# try. Leaving herdr (ctrl+b q) stops the server, the companion goes with it, and the temp
# folder is deleted.
#
#   url            optional: an agent-style `herdr browser open <url>` in the first pane
#                  (the companion starts on demand, in the background)
#   TRY_HEADLESS=1 a hidden self-check instead of the UI: the companion is never shown; an
#                  agent's show starts it on demand, a crash restarts it; exit status 0 = ok
#   TRY_KEEP=1     keep the temp folder (logs) after the run
#   TRY_INFO=file  write the temp folder, socket and binary to this file (for scripted smokes)
#   TRY_COMPANION_HEADLESS=1  the UI as usual, but the companion never shows a window (scripted smokes)
set -u
app=$(cd "$(dirname "$0")/.." && pwd)
wt=$(cd "$app/../.." && pwd)
built="$wt/target/release/herdr"
say() { printf 'try-launcher: %s\n' "$*"; }
die() { printf 'try-launcher: %s\n' "$*" >&2; exit 1; }

for v in HERDR_PANE_ID TMUX STY SSH_CONNECTION; do
  [ -n "${!v:-}" ] && die "$v is set: run this in a plain terminal, not inside herdr, tmux or ssh"
done
[ "$(uname)" = Darwin ] || say "not macOS: the dock and Browser view report \"unsupported\" here"
node=$(command -v node) || die "node not found (Node.js 22+)"

# ---------- build what is missing
if command -v cargo >/dev/null; then
  say "building herdr (release; a no-op when it is up to date)"
  (cd "$wt" && CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-8}" cargo build --release --locked -q) || die "the herdr build failed"
fi
[ -x "$built" ] || die "no herdr at $built (cargo build --release)"
if [ ! -d "$app/node_modules/playwright-core" ] || [ ! -d "$app/node_modules/electron" ]; then
  say "npm ci in apps/companion"
  (cd "$app" && npm ci --no-audit --no-fund --silent) || die "npm ci failed"
fi
if [ ! -d "$app/node_modules/electron/dist" ]; then
  say "downloading Electron into apps/companion/node_modules (once, about 115 MB)"
  (cd "$app" && "$node" node_modules/electron/install.js) || die "the Electron download failed"
fi

# ---------- the throwaway herdr
hooks=1 # the companion's test hooks: companion-crash below needs them
for v in $(env | sed -n 's/^\(HERDR_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$v"; done
T=$(mktemp -d /tmp/herdr-try-launcher.XXXXXX) || die "mktemp failed"
T=$(cd "$T" && pwd -P)
mkdir -p "$T/bin" "$T/home" "$T/config/herdr" "$T/state" "$T/data" "$T/cache"
cp -c "$built" "$T/bin/herdr" 2>/dev/null || cp "$built" "$T/bin/herdr" || die "cannot copy herdr"
sock="$T/config/herdr/herdr.sock"
real_home=$HOME
export HOME="$T/home" XDG_CONFIG_HOME="$T/config" XDG_STATE_HOME="$T/state" XDG_DATA_HOME="$T/data" XDG_CACHE_HOME="$T/cache"
export PATH="$T/bin:$PATH" REAL_HOME="$real_home" TRY_SOCKET="$sock" TRY_COMPANION_APP="$app"
export HERDR_COMPANION_ELECTRON_DIST="$app/node_modules/electron/dist" HERDR_COMPANION_TEST_HOOKS=$hooks
[ -n "${TRY_HEADLESS:-}${TRY_COMPANION_HEADLESS:-}" ] && export HERDR_COMPANION_HEADLESS=1

cat > "$T/config/herdr/config.toml" <<EOF
onboarding = false

[browser]
# the defaults, spelled out: the companion is herdr's browser, the herdr+ window is off,
# the companion starts on the first page an agent opens
window = false
host = "companion"
companion_autostart = false
node = "$node"
EOF
# The dock's geometry reads Ghostty's settings (padding, titlebar) the way your Ghostty does.
for f in "$real_home/.config/ghostty/config" "$real_home/Library/Application Support/com.mitchellh.ghostty/config"; do
  [ -f "$f" ] || continue
  case "$f" in
    "$real_home/.config/"*) dest="$T/config/ghostty/config" ;;
    *) dest="$T/home/Library/Application Support/com.mitchellh.ghostty/config" ;;
  esac
  mkdir -p "$(dirname "$dest")" && ln -s "$f" "$dest"
done
# The browser helper herdr drives the companion through: this checkout's playwright-core (the pin).
host_dir="$T/state/herdr/browser/host"
mkdir -p "$host_dir"
ln -s "$app/node_modules" "$host_dir/node_modules"
printf '{"version":1,"node":"%s","playwright_core":"1.63.0","assets_sha256":"","installed_at":0}\n' "$node" > "$host_dir/runtime.json"

cat > "$T/bin/companion-crash" <<'EOF'
#!/usr/bin/env node
// companion-crash: crash this throwaway herdr's companion (its test hook) to watch herdr restart it.
const path = require('node:path');
const app = process.env.TRY_COMPANION_APP;
const { Connection } = require(path.join(app, 'src', 'herdr-api.js'));
const { chromium } = require(path.join(app, 'node_modules', 'playwright-core'));
(async () => {
  const c = new Connection(process.env.TRY_SOCKET, { prefix: 'try', timeoutMs: 10000 });
  c.on('error', () => {});
  await c.connect();
  const info = (await c.request('browser.get', {}, { timeoutMs: 10000 })).browser || {};
  c.close();
  const p = (info.profiles || []).find((x) => x.name === 'companion' && x.state === 'running');
  if (!p || !p.port) throw new Error('the companion is not running');
  const b = await chromium.connectOverCDP(`http://127.0.0.1:${p.port}`, { timeout: 10000 });
  const page = b.contexts().flatMap((x) => x.pages()).find((x) => x.url().startsWith('herdr-companion://control/'));
  if (!page) throw new Error('no control page');
  await page.evaluate(() => globalThis.__herdrCompanion.__crash()).catch(() => {});
  await b.close().catch(() => {});
  console.log('companion-crash: crashed; herdr restarts it after 1 s (herdr browser companion status)');
})().catch((e) => {
  console.error(`companion-crash: ${e.message}`);
  process.exit(1);
});
EOF
chmod +x "$T/bin/companion-crash"

hx() { "$T/bin/herdr" "$@"; }

server_up() { pgrep -f "$T/bin/herdr server" >/dev/null 2>&1; }

cleanup() {
  local code=$?
  trap - EXIT INT TERM HUP
  # stop the server (again if a live handoff was under way: the new one answers then)
  for _ in 1 2 3 4 5; do
    hx server stop >/dev/null 2>&1
    for _ in $(seq 1 25); do server_up || break; sleep 0.2; done
    server_up || break
  done
  # the companion quits with the server; wait for every process of this run (never killed)
  local left=""
  for _ in $(seq 1 75); do
    left=$(pgrep -f "$T" 2>/dev/null | tr '\n' ' ')
    [ -z "$left" ] && break
    sleep 0.2
  done
  if [ -n "$left" ]; then
    say "still running (not killed): pid $left; kept $T"
  elif [ -n "${TRY_KEEP:-}" ]; then
    say "stopped; kept $T"
  else
    rm -rf "$T"
    say "stopped and cleaned up"
  fi
  exit $code
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP

say "installing the companion into the throwaway state dir (herdr browser setup --companion --yes)"
t0=$(date +%s)
hx browser setup --companion --yes > "$T/setup.out" 2>&1 || { sed 's/^/  /' "$T/setup.out"; die "setup --companion failed"; }
sed -n 's/^  \(companion: .*\)/  \1/p' "$T/setup.out"
say "installed in $(( $(date +%s) - t0 )) s"

hx server > "$T/server.log" 2>&1 &
for _ in $(seq 1 150); do [ -S "$sock" ] && hx browser companion status >/dev/null 2>&1 && break; sleep 0.1; done
[ -S "$sock" ] || die "the herdr server did not start (see $T/server.log)"
[ -n "${TRY_INFO:-}" ] && printf 'T=%s\nSOCK=%s\nBIN=%s\nAPP=%s\n' "$T" "$sock" "$T/bin/herdr" "$app" > "$TRY_INFO"

first_pane() { hx pane list 2>/dev/null | "$node" -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{try{console.log(JSON.parse(s).result.panes[0].pane_id)}catch{}})'; }
# The first pane, waiting up to $1 tenths of a second (a fresh server makes one once a client attaches).
wait_pane() {
  local p=""
  for _ in $(seq 1 "$1"); do p=$(first_pane); [ -n "$p" ] && break; sleep 0.1; done
  printf '%s' "$p"
}

# ---------- the hidden self-check
if [ -n "${TRY_HEADLESS:-}" ]; then
  fail=0
  check() { if [ "$1" = ok ]; then say "ok   $2"; else say "FAIL $2"; fail=1; fi; }
  hx browser doctor --json > "$T/doctor.json" 2>&1
  "$node" -e 'const c=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8")).find(x=>x.id==="companion_app");process.exit(c&&c.ok?0:1)' "$T/doctor.json" && r=ok || r=no
  check $r "doctor: the companion app is installed"
  pane=$(wait_pane 20)
  [ -z "$pane" ] && hx workspace create --label try >/dev/null 2>&1 && pane=$(wait_pane 50)
  [ -n "$pane" ] && r=ok || r=no
  check $r "a pane for the agent ($pane)"
  printf '<title>try-launcher</title><h1>herdr+ companion: on-demand start ok</h1>\n' > "$T/try.html"
  t0=$("$node" -e 'console.log(Date.now())')
  hx browser show "$T/try.html" --pane "$pane" --json > "$T/show.json" 2>&1
  ms=$(( $("$node" -e 'console.log(Date.now())') - t0 ))
  grep -q '"host":"companion"' "$T/show.json" && r=ok || r=no
  check $r "an agent's show started the companion on demand and landed there (${ms} ms)"
  status=$(hx browser companion status 2>&1 | head -1)
  case "$status" in "companion running"*) r=ok ;; *) r=no ;; esac
  check $r "$status"
  pid1=$(hx browser companion status --json 2>/dev/null | "$node" -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{try{const j=JSON.parse(s);console.log((j.result||j).companion.pid)}catch{}})')
  ps -o args= -p "$pid1" 2>/dev/null | grep -q -- '--background' && r=ok || r=no
  check $r "it runs in the background (--background), managed by herdr (pid $pid1)"
  hx browser read --pane "$pane" --format text > "$T/read.out" 2>&1
  grep -q 'on-demand start ok' "$T/read.out" && r=ok || r=no
  check $r "the agent reads its page"
  companion-crash > "$T/crash.out" 2>&1 && r=ok || r=no
  check $r "companion-crash crashed it"
  back=""
  for _ in $(seq 1 100); do
    s=$(hx browser companion status 2>&1 | head -1)
    case "$s" in "companion running"*) back=$s ;; esac
    pid2=$(hx browser companion status --json 2>/dev/null | "$node" -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{try{const j=JSON.parse(s);console.log((j.result||j).companion.pid)}catch{}})')
    [ -n "$back" ] && [ -n "$pid2" ] && [ "$pid2" != "$pid1" ] && break
    back=""
    sleep 0.1
  done
  [ -n "$back" ] && r=ok || r=no
  check $r "herdr restarted it: ${back:-not running again}"
  [ $fail -eq 0 ] && say "headless check passed" || say "headless check FAILED (logs: $T; TRY_KEEP=1 keeps them)"
  exit $fail
fi

# ---------- what to try, in the first pane
cat > "$T/TRY.txt" <<'EOF'

  herdr+ companion launcher · a throwaway herdr
  ctrl+b q leaves: the server, the companion and the
  temp folder go with it

  1  An agent opens a page; the companion starts on
     demand, in the background (no window, no focus):
       herdr browser open https://example.com
     ◎ marks the tab, the strip above the panes reads
     ▣ Terminal  ◎ Browser · 1 … ◨
  2  Dock: ctrl+b B, or ◨ (the strip's right end or
     the Browser row). Drag the divider to resize; it
     is remembered. ctrl+b B again undocks.
  3  Tabbed: click ◎ Browser in the strip (ctrl+b Z);
     ▣ Terminal goes back.
  4  The Browser row (bottom of the sidebar) shows the
     companion's state. Right-click: Focus / Restart /
     Stop companion. Click: the Browser overlay.
  5  Crash it: companion-crash. herdr restarts it
     after 1, 2, 4, 8 s; after 5 crashes in 5 min it
     waits for: herdr browser companion restart
  6  herdr browser companion status|restart|stop|log
  7  By hand: move, resize, minimise this window,
     another Space, full screen (the dock hides and
     comes back aligned); click the page to type in
     it, ⌘⎋ gives the keyboard back to herdr.
  8  A real agent: HOME=$REAL_HOME claude
     (your login lives in your real home)

EOF
for rc in .zshrc .bashrc; do
  printf '[ -f "%s/TRY.txt" ] && [ ! -e "%s/.try-shown" ] && { cat "%s/TRY.txt"; : > "%s/.try-shown"; }\nPS1="try %%~ %%# "\n' "$T" "$T" "$T" "$T" > "$T/home/$rc"
done
sed -i '' 's/^PS1="try %~ %# "$/PS1="try \\w \\$ "/' "$T/home/.bashrc" 2>/dev/null

if [ $# -ge 1 ]; then
  # once the UI made the first pane: an agent-style open there (the on-demand start)
  url=$1
  ( pane=$(wait_pane 300); [ -n "$pane" ] && sleep 1 && hx pane run "$pane" "herdr browser open $(printf '%q' "$url")" >/dev/null 2>&1 ) &
fi

say "$(hx --version 2>/dev/null | head -1) ($(git -C "$wt" rev-parse --abbrev-ref HEAD 2>/dev/null)@$(git -C "$wt" rev-parse --short HEAD 2>/dev/null)), isolated in $T"
say "starting its UI here; ctrl+b q leaves and cleans up"
sleep 1
# A live handoff ends the UI ("reconnect after handoff completes"): attach again while the
# server is up; ctrl+b q (status 0) or a stopped server ends the run.
for attach in 1 2 3 4 5 6; do
  hx && break
  for _ in $(seq 1 50); do hx browser companion status >/dev/null 2>&1 && break; sleep 0.1; done
  hx browser companion status >/dev/null 2>&1 || break
  say "the UI ended while the server is up (a live handoff?): attaching again"
  sleep 0.5
done
