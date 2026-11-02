#!/usr/bin/env bash
# End-to-end: the companion (headless Electron) against a REAL, fully isolated herdr server.
#
#   apps/companion/test/e2e-real-herdr.sh [--keep]
#
# * herdr: $HERDR_BIN, else <repo>/target/release/herdr (build it first: cargo build --release).
# * Isolation: HOME, XDG_CONFIG_HOME, XDG_STATE_HOME, XDG_DATA_HOME, XDG_CACHE_HOME all point into
#   a temp dir; every inherited HERDR_* variable is dropped (no HERDR_SOCKET_PATH /
#   HERDR_CLIENT_SOCKET_PATH), so the server's socket is <tmp>/config/herdr/herdr.sock and nothing
#   touches a live herdr.
# * The herdr+ window fallback can never open a window: [browser] executable points at a path
#   that does not exist, so "fell back to the window" is asserted from the routing decision
#   (the launch error), not from a launched Chromium.
# * The browser sidecar runtime is this app's node_modules (playwright-core 1.63.0, the pin).
# * Electron runs --headless (never shown, no dock icon, never focused). Everything is stopped
#   through its own channel (server.stop on the isolated socket, the companion's control page);
#   nothing is signalled. A process that does not stop is reported, not killed.
set -euo pipefail

APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$APP_DIR/../.." && pwd)"
HERDR_BIN="${HERDR_BIN:-$REPO/target/release/herdr}"
KEEP=0
[[ "${1:-}" == "--keep" ]] && KEEP=1

[[ -x "$HERDR_BIN" ]] || { echo "no herdr binary at $HERDR_BIN (cargo build --release, or set HERDR_BIN)" >&2; exit 2; }
[[ -d "$APP_DIR/node_modules/playwright-core" && -d "$APP_DIR/node_modules/electron" ]] || { echo "run npm install in $APP_DIR first" >&2; exit 2; }
NODE="$(command -v node)"

# Short: the live handoff socket lives under it (unix socket paths are limited to ~104 bytes).
T="$(mktemp -d /tmp/hcr-XXXXXX)"
T="$(cd "$T" && pwd -P)"
mkdir -p "$T/home" "$T/config/herdr" "$T/state" "$T/data" "$T/cache" "$T/bin" "$T/site"

# Drop every HERDR_* variable of the calling shell (a live herdr session's socket, pane id, ...).
for v in $(env | sed -n 's/^\(HERDR_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$v"; done
export HOME="$T/home" XDG_CONFIG_HOME="$T/config" XDG_STATE_HOME="$T/state" XDG_DATA_HOME="$T/data" XDG_CACHE_HOME="$T/cache"
export HERDR_E2E_TMP="$T" HERDR_E2E_BIN="$HERDR_BIN" HERDR_E2E_SOCKET="$T/config/herdr/herdr.sock"

# The Claude hook integration (in the temp HOME): herdr stores quiet messages (page errors)
# only for a Claude pane whose hook is current.
mkdir -p "$T/home/.claude"
"$HERDR_BIN" integration install claude >/dev/null

cat > "$T/config/herdr/config.toml" <<EOF
[browser]
enabled = true
autostart = false
host = "auto"
page_errors = "agent"
executable = "$T/no-chromium/herdr-e2e-chromium"
node = "$NODE"
EOF

# The sidecar runtime: herdr installs its scripts (assets differ => refresh in place) and needs
# node_modules/playwright-core at the pinned version.
HOST_DIR="$T/state/herdr/browser/host"
mkdir -p "$HOST_DIR"
ln -s "$APP_DIR/node_modules" "$HOST_DIR/node_modules"
printf '{"version":1,"node":"%s","playwright_core":"1.63.0","assets_sha256":"","installed_at":0}\n' "$NODE" > "$HOST_DIR/runtime.json"


start_server() {
  "$HERDR_BIN" server >>"$T/server.log" 2>&1 &
  echo $! >>"$T/server.pids"
  for _ in $(seq 1 100); do [[ -S "$HERDR_E2E_SOCKET" ]] && return 0; sleep 0.1; done
  echo "herdr server did not create its socket; log: $T/server.log" >&2
  return 1
}
export -f start_server 2>/dev/null || true

cleanup() {
  local code=$?
  "$HERDR_BIN" server stop >/dev/null 2>&1 || true
  sleep 1
  local left=""
  for p in $(cat "$T/server.pids" 2>/dev/null); do ps -p "$p" >/dev/null 2>&1 && left="$left $p"; done
  [[ -n "$left" ]] && echo "herdr server pid(s) still running:$left (not killed; stop them with HOME=$T/home XDG_CONFIG_HOME=$T/config $HERDR_BIN server stop)" >&2
  if [[ $code -eq 0 && $KEEP -eq 0 && -z "$left" ]]; then rm -rf "$T"; else echo "kept $T"; fi
  exit $code
}
trap cleanup EXIT

start_server
echo "isolated herdr: $HERDR_BIN  socket: $HERDR_E2E_SOCKET  tmp: $T"
cd "$APP_DIR"
"$NODE" test/e2e-real-herdr.mjs
