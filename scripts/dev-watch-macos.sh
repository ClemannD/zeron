#!/usr/bin/env bash
# Rebuild and relaunch Zeron Dev whenever a source file changes — the closest
# thing to hot reload for a native Rust app (state is not preserved; settings,
# open project tab and sessions are, since they persist in the dev data dir).
#
#   scripts/dev-watch-macos.sh            watch crates/ + apps/, relaunch on save
#
# Each change: build first while the old app keeps running. A failed build
# leaves it running and prints the errors; a good one quits Zeron Dev and
# relaunches it through scripts/run-macos-dev.sh (same bundle, data dir and
# IPC port, so it never touches your regular Zeron).
#
# Quitting Zeron Dev ends any agent sessions running inside it.
# Needs fswatch (brew install fswatch). Ctrl+C stops watching and quits the app.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"
BUNDLE_ID="${ZERON_DEV_BUNDLE_ID:-sh.zeron.app.dev}"
BIN="$ROOT/target/macos-dev/Zeron Dev.app/Contents/MacOS/zeron"
LAUNCHER_PID=""

if ! command -v fswatch >/dev/null 2>&1; then
  echo "fswatch not found: brew install fswatch" >&2
  exit 1
fi

dev_running() {
  pgrep -f -x "$BIN" >/dev/null 2>&1 || pgrep -f -x "$BIN .*" >/dev/null 2>&1
}

quit_dev() {
  dev_running || return 0
  osascript -e "tell application id \"$BUNDLE_ID\" to quit" >/dev/null 2>&1 || true
  for _ in $(seq 1 50); do
    dev_running || return 0
    sleep 0.1
  done
  echo "Zeron Dev did not quit in 5s; stopping it" >&2
  pkill -f -x "$BIN( .*)?" 2>/dev/null || true
  sleep 0.5
}

launch_dev() {
  # run-macos-dev.sh rebuilds (a no-op by now), refreshes the bundle and stays
  # attached until the app exits; keep it in the background.
  "$ROOT/scripts/run-macos-dev.sh" >/dev/null 2>&1 &
  LAUNCHER_PID=$!
}

rebuild() {
  local started=$SECONDS
  echo "── building ($(date +%H:%M:%S))" >&2
  if (cd "$ROOT" && cargo build -p zeron --message-format short 2>&1 |
    grep -E "^(error|warning: unused)|^crates/.*: error|^apps/.*: error|Finished" >&2; exit "${PIPESTATUS[0]}"); then
    quit_dev
    launch_dev
    echo "── relaunched Zeron Dev in $((SECONDS - started))s" >&2
  else
    echo "── build failed; Zeron Dev left as it was" >&2
  fi
}

cleanup() {
  echo >&2
  quit_dev
  [[ -n "$LAUNCHER_PID" ]] && kill "$LAUNCHER_PID" 2>/dev/null
  exit 0
}
trap cleanup INT TERM

rebuild
echo "── watching crates/ and apps/ (Ctrl+C to stop)" >&2
# -o: one line per batch of changes; -l: coalesce bursts (editor saves,
# formatters) into one rebuild.
fswatch -o -l 1 -r \
  -e '.*' -i '\.rs$' -i '\.toml$' -i '\.json$' -i '\.svg$' -i '\.png$' \
  "$ROOT/crates" "$ROOT/apps" |
  while read -r _; do
    rebuild
  done
