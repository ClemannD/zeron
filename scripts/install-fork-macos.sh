#!/usr/bin/env bash
# Build this fork as Zeron.app and install it over /Applications/Zeron.app.
#
#   scripts/install-fork-macos.sh            build + install
#   ZERON_FORK_BUILD_ONLY=1 scripts/install-fork-macos.sh
#                                            build target/package/Zeron.app only
#
# Differences from scripts/package-macos.sh (the upstream release packager):
#   - Auto-update is report-only (LSEnvironment ZERON_AUTO_UPDATE=0). Upstream
#     releases would otherwise download in the background and replace this
#     build on quit. An "update available" notice is the cue to merge
#     upstream/main and rerun this script.
#   - Talks to the self-hosted edge at https://zeron.clemann.app (sync and
#     passkey sign-in; see edge/wrangler.clemann.jsonc) instead of
#     edge.zeron.sh. ZERON_FORK_EDGE_URL overrides; empty keeps upstream's.
#   - No dmg, updater tarball, or notarization.
#   - Installs: refuses while Zeron is running, moves the current app to
#     target/package/Zeron.app.backup-<timestamp>, then copies the new one in.
#
# Sessions, settings, and credentials live outside the bundle, so they carry
# over in both directions. To roll back, quit Zeron and move a backup into
# /Applications as Zeron.app.
#
# Signing: ad-hoc unless CODESIGN_IDENTITY names a real identity (e.g. an
# "Apple Development: …" certificate). Ad-hoc builds may need right-click →
# Open on first launch, and macOS may ask again for microphone / screen
# recording access after each rebuild.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')"
COMMIT="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
OUT_DIR="$ROOT/target/package"
APP="$OUT_DIR/Zeron.app"
INSTALL_DIR="${ZERON_FORK_INSTALL_DIR:-/Applications}"
INSTALLED="$INSTALL_DIR/Zeron.app"

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found: install Rust (rustup) first" >&2
  exit 1
fi
if [[ -n "$(git -C "$ROOT" status --porcelain 2>/dev/null)" ]]; then
  echo "note: building with uncommitted changes on top of $COMMIT" >&2
fi

cd "$ROOT"
cargo build --release -p zeron

# Assemble the bundle exactly as scripts/package-macos.sh does.
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/licenses/fonts"
install -m 755 "$ROOT/target/release/zeron" "$APP/Contents/MacOS/zeron"
sed "s/__VERSION__/$VERSION/" "$ROOT/dist/macos/Info.plist" >"$APP/Contents/Info.plist"
cp "$ROOT/crates/ui/assets/fonts/licenses/"* "$APP/Contents/Resources/licenses/fonts/"
cp "$ROOT/THIRD_PARTY_NOTICES.md" "$ROOT/LICENSE" "$APP/Contents/Resources/licenses/"
cp "$ROOT/crates/voice/NOTICE.md" "$APP/Contents/Resources/licenses/parakeet-v3.txt"

ICONSET="$OUT_DIR/zeron.iconset"
rm -rf "$ICONSET" && mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$ROOT/dist/macos/icon-1024.png" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  retina=$((size * 2))
  sips -z "$retina" "$retina" "$ROOT/dist/macos/icon-1024.png" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/zeron.icns"
rm -rf "$ICONSET"

# Fork-only: report-only updates, and record which commit this build is.
PLIST="$APP/Contents/Info.plist"
plutil -insert LSEnvironment -dictionary "$PLIST" 2>/dev/null || true
plutil -replace LSEnvironment.ZERON_AUTO_UPDATE -string "0" "$PLIST"
# Self-hosted edge (edge/wrangler.clemann.jsonc): sync and sign-in go to our
# own Worker, which is also the identity provider (AUTH_MODE=self), so the
# sign-in page lives at the edge too. ZERON_FORK_EDGE_URL="" keeps upstream's.
FORK_EDGE="${ZERON_FORK_EDGE_URL-https://zeron.clemann.app}"
if [[ -n "$FORK_EDGE" ]]; then
  plutil -replace LSEnvironment.ZERON_EDGE_URL -string "$FORK_EDGE" "$PLIST"
  plutil -replace LSEnvironment.ZERON_WORKOS_API_BASE -string "$FORK_EDGE" "$PLIST"
  plutil -replace LSEnvironment.ZERON_WORKOS_CLIENT_ID -string "${ZERON_FORK_CLIENT_ID:-zeron-self}" "$PLIST"
fi
plutil -replace ZeronForkCommit -string "$COMMIT" "$PLIST"

# Sign last: any edit to the bundle after this invalidates the signature.
if [[ -n "${CODESIGN_IDENTITY:-}" ]]; then
  codesign --entitlements "$ROOT/dist/macos/Zeron.entitlements" --force --options runtime \
    --sign "$CODESIGN_IDENTITY" "$APP"
else
  codesign --entitlements "$ROOT/dist/macos/Zeron.entitlements" --force --sign - "$APP"
fi
codesign --verify --strict "$APP"
echo "built: $APP ($VERSION, fork commit $COMMIT)" >&2

if [[ "${ZERON_FORK_BUILD_ONLY:-0}" == "1" ]]; then
  exit 0
fi

# Only the headed app must be stopped before swapping the bundle. The managed
# engine intentionally outlives its window so sessions can continue, and it
# knows how to restart from the installed path after an update. Likewise,
# `zeron mcp` is a stateless helper launched by Codex.
#
# `pgrep -f "$BIN .*"` used to catch both of those long-lived processes and
# made a local Codex MCP session—or the intentional background engine—look
# like a running Zeron app after the user quit its window.
BIN="$INSTALLED/Contents/MacOS/zeron"
BLOCKERS=()
while IFS=$'\t' read -r pid command; do
  case "$command" in
    "$BIN")
      BLOCKERS+=("$pid  $command")
      ;;
  esac
done < <(ps -axo pid=,command= | sed -E 's/^[[:space:]]*([0-9]+)[[:space:]]+(.*)$/\1\t\2/')

if (( ${#BLOCKERS[@]} > 0 )); then
  echo "Zeron is still running:" >&2
  printf '  %s\n' "${BLOCKERS[@]}" >&2
  echo "Quit Zeron (⌘Q) and rerun this script." >&2
  exit 1
fi

if [[ -d "$INSTALLED" ]]; then
  BACKUP="$OUT_DIR/Zeron.app.backup-$(date +%Y%m%d-%H%M%S)"
  mv "$INSTALLED" "$BACKUP"
  echo "previous app moved to: $BACKUP" >&2
fi
ditto "$APP" "$INSTALLED"
echo "installed: $INSTALLED" >&2
echo "launch it from Applications (first launch of an ad-hoc build: right-click → Open)." >&2
