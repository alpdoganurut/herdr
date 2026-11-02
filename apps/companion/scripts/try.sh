#!/bin/bash
# Try the herdr+ companion browser against an isolated herdr, in one command.
# Run it in a plain terminal (not inside herdr, tmux or ssh). It never touches your live herdr:
# a throwaway herdr (temp HOME/XDG dirs, its own socket) starts here, the companion window opens
# next to it, and leaving herdr (ctrl+b q) quits the companion, stops the throwaway server and
# deletes its temp folder.
#
# Usage: try.sh [url]        (url: optional first page, opened in the first herdr tab)
# In a pane: `herdr browser open <url>`, `herdr browser show <file.html>`;
# for an agent: `HOME=$REAL_HOME claude` (your login lives in your real home).
set -u
app=$(cd "$(dirname "$0")/.." && pwd)
wt=$(cd "$app/../.." && pwd)
herdr_bin="$wt/target/release/herdr"
die() { printf 'try.sh: %s\n' "$*" >&2; exit 1; }

for v in HERDR_PANE_ID TMUX STY SSH_CONNECTION; do
  [ -n "${!v:-}" ] && die "$v is set: run this in a plain terminal, not inside herdr/tmux/ssh"
done
command -v node >/dev/null || die "node not found"

# Build what is missing.
[ -x "$herdr_bin" ] || { echo "try.sh: building herdr (first run takes a few minutes)"; (cd "$wt" && cargo build --release -q) || die "herdr build failed"; }
[ -d "$app/node_modules" ] || { echo "try.sh: npm install"; (cd "$app" && npm install --silent) || die "npm install failed"; }
[ -d "$app/node_modules/electron/dist" ] || (cd "$app" && node node_modules/electron/install.js) || die "electron download failed"

T=$(mktemp -d /tmp/herdr-companion-try.XXXXXX) || die "mktemp failed"
H="$T/state/herdr/browser/host"
mkdir -p "$T/config/herdr" "$H"
printf 'onboarding = false\n\n[browser]\nenabled = true\nhost = "auto"\n' > "$T/config/herdr/config.toml"
ln -s "$app/node_modules" "$H/node_modules"
printf '{"version":1,"node":"%s","playwright_core":"1.63.0","assets_sha256":"","installed_at":0}\n' "$(command -v node)" > "$H/runtime.json"
sock="$T/config/herdr/herdr.sock"

hx() {
  env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH -u HERDR_PANE_ID \
    HOME="$T" REAL_HOME="$HOME" XDG_CONFIG_HOME="$T/config" XDG_STATE_HOME="$T/state" \
    PATH="$(dirname "$herdr_bin"):$PATH" "$herdr_bin" "$@"
}

companion_pid=""
cleanup() {
  [ -n "$companion_pid" ] && kill -TERM "$companion_pid" 2>/dev/null
  hx server stop >/dev/null 2>&1
  for _ in $(seq 1 50); do pgrep -f "$T" >/dev/null 2>&1 || break; sleep 0.2; done
  rm -rf "$T"
  printf 'try.sh: stopped and cleaned up\n'
}
trap cleanup EXIT INT TERM

hx server > "$T/server.out" 2>&1 &
for _ in $(seq 1 100); do [ -S "$sock" ] && break; sleep 0.1; done
[ -S "$sock" ] || die "herdr server did not start (see $T/server.out)"

(cd "$app" && exec env -u HERDR_SOCKET_PATH -u HERDR_PANE_ID npx --no-install electron . --socket "$sock" --profile-dir "$T/companion" ${TRY_HEADLESS:+--headless}) > "$T/companion.log" 2>&1 &
companion_pid=$!
for _ in $(seq 1 120); do grep -q -E 'lease |error' "$T/companion.log" 2>/dev/null && break; sleep 0.25; done
grep -q 'lease ' "$T/companion.log" || printf 'try.sh: the companion has not registered yet (log: %s)\n' "$T/companion.log"

if [ $# -ge 1 ]; then
  ( sleep 2; p=$(hx pane list 2>/dev/null | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{try{console.log(JSON.parse(s).result.panes[0].pane_id)}catch{}})')
    [ -n "$p" ] && hx browser open "$1" --pane "$p" >/dev/null 2>&1 ) &
fi

printf 'try.sh: companion log %s\n' "$T/companion.log"
[ -n "${TRY_HEADLESS:-}" ] && { hx browser status | grep "profile companion"; exit 0; }
hx    # the herdr UI, in this terminal; ctrl+b q leaves and cleans up
