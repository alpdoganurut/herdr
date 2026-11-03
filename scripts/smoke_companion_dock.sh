#!/usr/bin/env bash
# Live dock alignment smoke (launcher design §10.2): the companion's dock window over a real
# Ghostty window, measured.
#
#   scripts/smoke_companion_dock.sh [--variants small,defaults,large,padding,balanced,native,hidden,clicks] [--report <file>] [--keep]
#
# For each variant a SEPARATE background Ghostty instance (never the user's) runs an isolated
# herdr client that docks the browser on its own (HERDR_TEST_BROWSER_VIEW=side); the companion
# (installed from this build into the temp state dir, managed, test hooks on) covers the band.
# The smoke reads both windows from CGWindowList (no permission) through a small Swift reader it
# compiles into the temp dir (not the companion's osascript probe; JXA only without swiftc),
# recomputes the band from the client's placement and Ghostty's measured frame
# (apps/companion/src/dock/geometry.js) and records the error per edge in points and device
# pixels, plus the window probe's CPU while docked and idle. small, defaults and large are the
# default Ghostty config at three window sizes; the rest are geometry configs. `clicks` is the
# defaults plus the z-order across clicks into the terminal (Ghostty's own "activate window" over
# Apple Events, to the test instance only and only when Automation is already allowed) and
# another app's window coming to the front, sampled every 2 ms: the dock is never below the
# terminal, and the other app's window covers it.
#
# Ghostty creates its first window only when the instance is activated: each variant brings its
# own instance to the front for a few seconds (the user's windows are never touched, focused,
# typed into or closed). Everything stops through its own channel: the server with `server
# stop` on its socket (the companion quits with it, the client exits, the Ghostty instance quits
# after its last window); a process that lingers is recorded, never killed.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
APP="$REPO/apps/companion"
BIN="${HERDR_BIN:-$REPO/target/debug/herdr}"
VARIANTS="small,defaults,large,padding,balanced,native,hidden"
REPORT=""
KEEP=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --variants) VARIANTS="$2"; shift 2 ;;
    --report) REPORT="$2"; shift 2 ;;
    --keep) KEEP=1; shift ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
done
[[ "$(uname)" == "Darwin" ]] || { echo "the dock is macOS only" >&2; exit 2; }
[[ -x "$BIN" ]] || { echo "no herdr binary at $BIN (cargo build, or set HERDR_BIN)" >&2; exit 2; }
[[ -d /Applications/Ghostty.app ]] || { echo "no /Applications/Ghostty.app" >&2; exit 2; }
[[ -d "$APP/node_modules/electron/dist" && -d "$APP/node_modules/playwright-core" ]] || { echo "run npm ci in $APP first" >&2; exit 2; }
NODE="$(command -v node)"

T="$(mktemp -d /tmp/hcd-XXXXXX)"
T="$(cd "$T" && pwd -P)"
for v in $(env | sed -n 's/^\(HERDR_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$v"; done
export HERDR_SMOKE_TMP="$T" HERDR_SMOKE_BIN="$BIN" HERDR_SMOKE_APP="$APP" HERDR_SMOKE_VARIANTS="$VARIANTS"
export HERDR_SMOKE_REPORT="${REPORT:-/tmp/herdr-dock-smoke-$(date +%Y%m%d-%H%M%S).md}"
echo "dock alignment smoke: $BIN  tmp: $T"
code=0
"$NODE" "$REPO/scripts/smoke_companion_dock.mjs" || code=$?
# The temp dir holds an installed companion (~500 MB): gone unless --keep or something
# of the run still uses it (then it is reported, never killed).
if [[ $KEEP -eq 0 ]]; then
  if pgrep -f "$T" >/dev/null 2>&1; then echo "kept $T: a process of the run still uses it" >&2; else rm -rf "$T"; fi
else
  echo "kept $T"
fi
exit $code
