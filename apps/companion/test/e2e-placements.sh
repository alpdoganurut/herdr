#!/usr/bin/env bash
# End-to-end: the browser dock against a REAL, fully isolated herdr: a herdr TUI client in a
# pseudo-terminal (test/pty-client.py, a chosen grid and pixel size) docks the browser, the
# companion (this checkout, headless, test hooks on) covers the placement over a fake terminal
# window (HERDR_COMPANION_FAKE_WINDOWS) and reports back.
#
#   apps/companion/test/e2e-placements.sh [--keep]
#
# * herdr: $HERDR_BIN, else <repo>/target/release/herdr, else <repo>/target/debug/herdr.
# * Isolation as in e2e-launcher.sh: HOME and every XDG dir in a temp dir, every inherited
#   HERDR_* variable dropped; nothing touches a live herdr, no window is shown, nothing is killed.
set -euo pipefail

APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$APP_DIR/../.." && pwd)"
BIN="${HERDR_BIN:-}"
if [[ -z "$BIN" ]]; then
  BIN="$REPO/target/release/herdr"
  [[ -x "$BIN" ]] || BIN="$REPO/target/debug/herdr"
fi
KEEP=0
[[ "${1:-}" == "--keep" ]] && KEEP=1
[[ -x "$BIN" ]] || { echo "no herdr binary at $BIN (cargo build, or set HERDR_BIN)" >&2; exit 2; }
[[ -d "$APP_DIR/node_modules/playwright-core" && -d "$APP_DIR/node_modules/electron/dist" ]] || {
  echo "run npm ci in $APP_DIR first" >&2
  exit 2
}
NODE="$(command -v node)"
PY="$(command -v python3)"

T="$(mktemp -d /tmp/hcp-XXXXXX)"
T="$(cd "$T" && pwd -P)"
mkdir -p "$T/home" "$T/config/herdr" "$T/state" "$T/data" "$T/cache"

for v in $(env | sed -n 's/^\(HERDR_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$v"; done
export HOME="$T/home" XDG_CONFIG_HOME="$T/config" XDG_STATE_HOME="$T/state" XDG_DATA_HOME="$T/data" XDG_CACHE_HOME="$T/cache"
# A debug build keeps its config and sockets under herdr-dev/, a release build under herdr/.
export HERDR_E2E_TMP="$T" HERDR_E2E_BIN="$BIN" HERDR_E2E_PYTHON="$PY"
export HERDR_E2E_SOCKETS="$T/config/herdr/herdr.sock:$T/config/herdr-dev/herdr.sock"
mkdir -p "$T/config/herdr-dev"

cat > "$T/config/herdr/config.toml" <<CONF
onboarding = false

[browser]
enabled = true
autostart = false
window = false
host = "companion"
executable = "$T/no-chromium/herdr-e2e-chromium"
node = "$NODE"

[ui]
sidebar_layout = "tabs"
CONF

cp "$T/config/herdr/config.toml" "$T/config/herdr-dev/config.toml"

for state in herdr herdr-dev; do
  HOST_DIR="$T/state/$state/browser/host"
  mkdir -p "$HOST_DIR"
  ln -s "$APP_DIR/node_modules" "$HOST_DIR/node_modules"
  printf '{"version":1,"node":"%s","playwright_core":"1.63.0","assets_sha256":"","installed_at":0}\n' "$NODE" > "$HOST_DIR/runtime.json"
done

cleanup() {
  local code=$?
  echo quit >> "$T/client.commands" 2>/dev/null || true
  # the companion this run started: quit through its control page (headless allows it)
  "$NODE" "$APP_DIR/test/e2e-quit.mjs" "$T/profile" >/dev/null 2>&1 || true
  "$BIN" server stop >/dev/null 2>&1 || true
  sleep 1
  local left=""
  for p in $(cat "$T/pids" 2>/dev/null); do ps -p "$p" >/dev/null 2>&1 && left="$left $p"; done
  [[ -n "$left" ]] && echo "still running:$left (not killed; the e2e's own processes, see $T)" >&2
  if [[ $code -eq 0 && $KEEP -eq 0 && -z "$left" ]]; then rm -rf "$T"; else echo "kept $T"; fi
  exit $code
}
trap cleanup EXIT

echo "isolated herdr: $BIN  tmp: $T"
cd "$APP_DIR"
"$NODE" test/e2e-placements.mjs
