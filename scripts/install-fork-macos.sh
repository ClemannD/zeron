#!/usr/bin/env bash
# Build this fork as Zeron.app (the zgx interface plus its engine) and install
# it over /Applications/Zeron.app.
#
#   scripts/install-fork-macos.sh            build + install
#   ZERON_FORK_BUILD_ONLY=1 scripts/install-fork-macos.sh
#                                            build target/package/Zeron.app only
#
# This mirrors upstream's scripts/package-zgx-macos.sh (bundle layout, plist
# keys, resources, signing order). Diff that script on every upstream merge and
# carry its changes over. It differs in what makes it ours:
#   - The interface talks to the self-hosted edge at https://zeron.clemann.app
#     (sync and passkey sign-in; see edge/wrangler.clemann.jsonc) instead of
#     edge.zeron.sh. zgx's release build ignores environment variables, so the
#     edge and sign-in client are baked in at compile time (apps/zgx/src/auth.rs,
#     ZERON_FORK_EDGE_URL / ZERON_FORK_CLIENT_ID). ZERON_FORK_EDGE_URL="" keeps
#     upstream's.
#   - Versions: upstream's packager accepts only x.y.z or x.y.z-beta.n. A
#     private release (ZERON_FORK_BUILD=n, scripts/release-fork-macos.sh) is
#     x.y.z.n everywhere: `zeron --version`, the plist, and the updater.
#   - Auto-update is report-only unless ZERON_FORK_BUILD is set, in which case
#     it follows the private feed (docs/macos-private-updates.md).
#   - No dmg or notarization here (release-fork-macos.sh notarizes).
#   - Installs: refuses while Zeron is running, moves the current app to
#     target/package/Zeron.app.backup-<timestamp>, then copies the new one in.
#
# Sessions, settings, and credentials live outside the bundle (~/.zeron), so
# they carry over in both directions. To roll back, quit Zeron and move a
# backup into /Applications as Zeron.app.
#
# Signing: ad-hoc unless CODESIGN_IDENTITY names a real identity. Ad-hoc builds
# may need right-click → Open on first launch, and macOS may ask again for
# microphone / screen recording access after each rebuild.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"
# zgx's terminal (zgx-ghostty-sys) builds Ghostty with Zig 0.16, which
# Homebrew installs keg-only.
[[ -z "${ZIG:-}" && -x /opt/homebrew/opt/zig@0.16/bin/zig ]] && PATH="/opt/homebrew/opt/zig@0.16/bin:$PATH"
die() { echo "$*" >&2; exit 1; }
command -v cargo >/dev/null 2>&1 || die "cargo not found: install Rust (rustup) first"
command -v zig >/dev/null 2>&1 || [[ -n "${ZIG:-}" ]] || die "zig not found: brew install zig@0.16"

VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')"
COMMIT="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
OUT_DIR="$ROOT/target/package"
APP="$OUT_DIR/Zeron.app"
INSTALL_DIR="${ZERON_FORK_INSTALL_DIR:-/Applications}"
INSTALLED="$INSTALL_DIR/Zeron.app"
ENTITLEMENTS="$ROOT/dist/macos/Zeron.entitlements"
FORK_EDGE="${ZERON_FORK_EDGE_URL-https://zeron.clemann.app}"
FORK_CLIENT="${ZERON_FORK_CLIENT_ID:-zeron-self}"
[[ -n "$FORK_EDGE" ]] || FORK_CLIENT=""

APP_VERSION="$VERSION"
if [[ -n "${ZERON_FORK_BUILD:-}" ]]; then
  [[ "$ZERON_FORK_BUILD" =~ ^[1-9][0-9]*$ ]] || die "ZERON_FORK_BUILD must be a positive integer"
  APP_VERSION="$VERSION.$ZERON_FORK_BUILD"
fi
if [[ -n "$(git -C "$ROOT" status --porcelain 2>/dev/null)" ]]; then
  echo "note: building with uncommitted changes on top of $COMMIT" >&2
fi

# Read at build time by both binaries: the private feed (crates/update/src/fork.rs)
# and the edge and sign-in client (apps/zgx/src/auth.rs for the interface,
# crates/cli/src/env.rs for `zeron`'s own commands and the engine's defaults).
export ZERON_FORK_BUILD="${ZERON_FORK_BUILD:-}"
export ZERON_FORK_UPDATE_TOKEN="${ZERON_FORK_UPDATE_TOKEN:-}"
export ZERON_FORK_EDGE_URL="$FORK_EDGE" ZERON_FORK_CLIENT_ID="$FORK_CLIENT"

# The engine, built as releases build it (no Luau asserts).
(cd "$ROOT" && LUAU_CXXFLAGS=-DNDEBUG cargo build --release -p zeron --bin zeron-engine)
ENGINE_BIN="$ROOT/target/release/zeron-engine"

# The interface: apps/zgx is a workspace of its own. Built from its folder so
# its .cargo/config.toml (LUAU_CXXFLAGS among others) applies. The `beta`
# feature is what makes it Zeron.app (production endpoints, the bundle's
# engine); ZERON_BETA_VERSION only sets the version it reports.
ZGX_TARGET_DIR="${ZGX_TARGET_DIR:-$ROOT/apps/zgx/target}"
(cd "$ROOT/apps/zgx" && \
  ZERON_BETA_VERSION="$APP_VERSION" \
  cargo build --release --features beta --target-dir "$ZGX_TARGET_DIR")
ZGX_BIN="$ZGX_TARGET_DIR/release/zeron-zgx"

[[ "$("$ZGX_BIN" --version)" == "zeron $APP_VERSION" ]] ||
  die "$ZGX_BIN isn't the $APP_VERSION app (it says '$("$ZGX_BIN" --version)')"
! LC_ALL=C grep -aq "Luau was built with its asserts" "$ZGX_BIN" ||
  die "$ZGX_BIN was built with Luau's asserts"
if [[ -n "$FORK_EDGE" ]]; then
  for bin in "$ZGX_BIN" "$ENGINE_BIN"; do
    for mark in "$FORK_EDGE" "$FORK_CLIENT"; do
      LC_ALL=C grep -aqF "$mark" "$bin" || die "$bin doesn't carry $mark: the fork endpoint wasn't baked in"
    done
  done
  # The interface picks the engine's sign-in client: upstream's mustn't be in
  # it. (The engine keeps a copy only Android's sign-in reaches,
  # crates/client/src/auth.rs.)
  ! LC_ALL=C grep -aqF "client_01KWD0EAKZKD50YCQJNYSRE4BY" "$ZGX_BIN" ||
    die "$ZGX_BIN still carries upstream's sign-in client"
fi

# Assemble the bundle as scripts/package-zgx-macos.sh does.
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
install -m 755 "$ZGX_BIN" "$APP/Contents/MacOS/zeron"
install -m 755 "$ENGINE_BIN" "$APP/Contents/MacOS/Zeron Engine"
PLIST="$APP/Contents/Info.plist"
sed "s/__VERSION__/$VERSION/" "$ROOT/dist/macos/Info.plist" >"$PLIST"
plutil -replace ZeronEngineVersion -string "$APP_VERSION" "$PLIST"
plutil -replace ZeronVersion -string "$APP_VERSION" "$PLIST"
plutil -replace LSMinimumSystemVersion -string "13.0" "$PLIST"

rsync -a --exclude '.zgx/' --exclude '/tests/' --exclude '.lock' --exclude '.DS_Store' \
  "$ROOT/apps/zgx/ui/" "$APP/Contents/Resources/ui/"
rsync -a --exclude '.zgx/' --exclude '.DS_Store' \
  "$ROOT/apps/zgx/plugins-builtin/" "$APP/Contents/Resources/plugins-builtin/"
for dir in "$APP/Contents/Resources/plugins-builtin/"*/; do
  cp "$ROOT/apps/zgx/plugins-sdk/zeron.luau" "$dir/zeron.luau"
done
mkdir -p "$APP/Contents/Resources/licenses/fonts"
cp "$ROOT/assets/fonts/licenses/"* "$ROOT/apps/zgx/fonts/"*-OFL.txt "$APP/Contents/Resources/licenses/fonts/"
cp "$ROOT/THIRD_PARTY_NOTICES.md" "$ROOT/LICENSE" "$APP/Contents/Resources/licenses/"
cp "$ROOT/crates/voice/NOTICE.md" "$APP/Contents/Resources/licenses/parakeet-v3.txt"
"$ROOT/scripts/macos-app-icon.sh" "$APP/Contents/Resources"

# Fork-only plist keys: update policy, the self-hosted edge for anything run
# from the bundle (the `zeron` command line), and which commit this is.
plutil -insert LSEnvironment -dictionary "$PLIST" 2>/dev/null || true
plutil -replace LSEnvironment.ZERON_AUTO_UPDATE -string "0" "$PLIST"
if [[ -n "$ZERON_FORK_BUILD" ]]; then
  # CFBundleVersion only goes up: LaunchServices takes the highest of several
  # copies with one bundle id, and a fork build number beats upstream's x.y.z.n.
  plutil -replace CFBundleVersion -string "$ZERON_FORK_BUILD" "$PLIST"
  plutil -replace ZeronUpdateVersion -string "$APP_VERSION" "$PLIST"
  plutil -replace LSEnvironment.ZERON_RELEASES_URL -string "https://zeron.clemann.app/fork-updates" "$PLIST"
  plutil -replace LSEnvironment.ZERON_AUTO_UPDATE -string "1" "$PLIST"
else
  plutil -replace CFBundleVersion -string "$VERSION.9999" "$PLIST"
fi
if [[ -n "$FORK_EDGE" ]]; then
  plutil -replace LSEnvironment.ZERON_EDGE_URL -string "$FORK_EDGE" "$PLIST"
  plutil -replace LSEnvironment.ZERON_WORKOS_API_BASE -string "$FORK_EDGE" "$PLIST"
  plutil -replace LSEnvironment.ZERON_WORKOS_CLIENT_ID -string "$FORK_CLIENT" "$PLIST"
fi
plutil -replace ZeronForkCommit -string "$COMMIT" "$PLIST"
plutil -convert xml1 "$PLIST"
plutil -lint "$PLIST" >/dev/null

# Sign last, inside out: any edit to the bundle after this invalidates it.
if [[ -n "${CODESIGN_IDENTITY:-}" ]]; then
  codesign --entitlements "$ENTITLEMENTS" --force --options runtime --timestamp \
    --identifier sh.zeron.app.engine --sign "$CODESIGN_IDENTITY" "$APP/Contents/MacOS/Zeron Engine"
  codesign --entitlements "$ENTITLEMENTS" --force --options runtime --timestamp \
    --sign "$CODESIGN_IDENTITY" "$APP"
else
  codesign --entitlements "$ENTITLEMENTS" --force \
    --identifier sh.zeron.app.engine --sign - "$APP/Contents/MacOS/Zeron Engine"
  codesign --entitlements "$ENTITLEMENTS" --force --sign - "$APP"
fi
codesign --verify --strict "$APP"
echo "built: $APP ($APP_VERSION, fork commit $COMMIT)" >&2

if [[ "${ZERON_FORK_BUILD_ONLY:-0}" == "1" ]]; then
  exit 0
fi

# Only the app window must be stopped before swapping the bundle. The managed
# engine intentionally outlives its window and restarts from the installed
# path, and `zeron mcp` is a stateless helper launched by agents.
BIN="$INSTALLED/Contents/MacOS/zeron"
BLOCKERS=()
while IFS=$'\t' read -r pid command; do
  case "$command" in
    "$BIN") BLOCKERS+=("$pid  $command") ;;
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
