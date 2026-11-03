#!/usr/bin/env bash
# End-to-end: herdr's companion launcher against a REAL, fully isolated herdr server.
#
#   apps/companion/test/e2e-launcher.sh [--keep]
#
# * herdr: $HERDR_BIN, else <repo>/target/release/herdr (build it first: cargo build --release).
# * Isolation: HOME, XDG_CONFIG_HOME, XDG_STATE_HOME, XDG_DATA_HOME, XDG_CACHE_HOME all point into
#   a temp dir; every inherited HERDR_* variable is dropped, so the server's socket is
#   <tmp>/config/herdr/herdr.sock and nothing touches a live herdr.
# * `herdr browser setup --companion --yes` installs the companion into the temp state dir, cloning
#   this app's node_modules/electron/dist (HERDR_COMPANION_ELECTRON_DIST): no download, ever.
# * The companion herdr launches runs headless (HERDR_COMPANION_HEADLESS=1, inherited from the
#   server: never shown, no Dock icon, never focused) with its test hooks on (__crash/__hang).
# * Everything is stopped through its own channel (the companion through herdr's stop, the
#   server with `server stop` on its socket); a process that does not stop is reported, not killed.
set -euo pipefail

APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$APP_DIR/../.." && pwd)"
HERDR_BIN="${HERDR_BIN:-$REPO/target/release/herdr}"
KEEP=0
[[ "${1:-}" == "--keep" ]] && KEEP=1

[[ -x "$HERDR_BIN" ]] || { echo "no herdr binary at $HERDR_BIN (cargo build --release, or set HERDR_BIN)" >&2; exit 2; }
[[ -d "$APP_DIR/node_modules/playwright-core" && -d "$APP_DIR/node_modules/electron/dist" ]] || {
  echo "run npm ci in $APP_DIR first (and node node_modules/electron/install.js if node_modules/electron/dist is missing)" >&2
  exit 2
}
NODE="$(command -v node)"

# Short: the live handoff socket lives under it (unix socket paths are limited to ~104 bytes).
T="$(mktemp -d /tmp/hcl-XXXXXX)"
T="$(cd "$T" && pwd -P)"
mkdir -p "$T/home" "$T/config/herdr" "$T/state" "$T/data" "$T/cache"

for v in $(env | sed -n 's/^\(HERDR_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$v"; done
export HOME="$T/home" XDG_CONFIG_HOME="$T/config" XDG_STATE_HOME="$T/state" XDG_DATA_HOME="$T/data" XDG_CACHE_HOME="$T/cache"
export HERDR_E2E_TMP="$T" HERDR_E2E_BIN="$HERDR_BIN" HERDR_E2E_SOCKET="$T/config/herdr/herdr.sock"
# Inherited by the companion the server launches.
export HERDR_COMPANION_HEADLESS=1 HERDR_COMPANION_TEST_HOOKS=1
export HERDR_COMPANION_ELECTRON_DIST="$APP_DIR/node_modules/electron/dist"

cat > "$T/config/herdr/config.toml" <<EOF
[browser]
enabled = true
autostart = false
window = false
host = "companion"
executable = "$T/no-chromium/herdr-e2e-chromium"
node = "$NODE"
EOF

# The sidecar runtime: this app's node_modules (playwright-core at the pin).
HOST_DIR="$T/state/herdr/browser/host"
mkdir -p "$HOST_DIR"
ln -s "$APP_DIR/node_modules" "$HOST_DIR/node_modules"
printf '{"version":1,"node":"%s","playwright_core":"1.63.0","assets_sha256":"","installed_at":0}\n' "$NODE" > "$HOST_DIR/runtime.json"

cleanup() {
  local code=$?
  "$HERDR_BIN" server stop >/dev/null 2>&1 || true
  sleep 1
  local left=""
  for p in $(cat "$T/server.pids" 2>/dev/null); do ps -p "$p" >/dev/null 2>&1 && left="$left $p"; done
  local companion=""
  if [[ -f "$T/state/herdr/browser/companion-app/run.json" ]]; then
    companion="$(sed -n 's/.*"pid": *\([0-9]*\).*/\1/p' "$T/state/herdr/browser/companion-app/run.json" | head -1)"
    [[ -n "$companion" ]] && ! ps -p "$companion" >/dev/null 2>&1 && companion=""
  fi
  [[ -n "$left" ]] && echo "herdr server pid(s) still running:$left (not killed; stop them with HOME=$T/home XDG_CONFIG_HOME=$T/config $HERDR_BIN server stop)" >&2
  [[ -n "$companion" ]] && echo "companion pid $companion still running (not killed)" >&2
  if [[ $code -eq 0 && $KEEP -eq 0 && -z "$left" && -z "$companion" ]]; then rm -rf "$T"; else echo "kept $T"; fi
  exit $code
}
trap cleanup EXIT

echo "isolated herdr: $HERDR_BIN  socket: $HERDR_E2E_SOCKET  tmp: $T"
cd "$APP_DIR"
"$NODE" test/e2e-launcher.mjs
