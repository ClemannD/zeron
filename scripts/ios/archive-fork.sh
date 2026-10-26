#!/usr/bin/env bash
# Build this fork for TestFlight. No signing credentials are needed to archive.
#   scripts/ios/archive-fork.sh
#   BUILD_NUMBER=2 scripts/ios/archive-fork.sh upload
# Upload uses the saved local Apple config, Xcode's account, or AC_* env vars.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ACTION="${1:-archive}"
case "$ACTION" in
  archive|upload) ;;
  *) echo "usage: $0 [archive|upload]" >&2; exit 2 ;;
esac
BUILD_NUMBER="${BUILD_NUMBER:-1}"
if [[ ! "$BUILD_NUMBER" =~ ^[1-9][0-9]*$ ]]; then
  echo "BUILD_NUMBER must be a positive integer" >&2
  exit 2
fi

OUT="$ROOT/target/ios-fork"
ARCHIVE="$OUT/Zeron.xcarchive"
CONFIG="$ROOT/apps/ios/Fork.clemann.xcconfig"
mkdir -p "$OUT"
cd "$ROOT"
export PATH="$HOME/.cargo/bin:$PATH"

xcodebuild -project apps/ios/Zeron.xcodeproj -scheme Zeron \
  -xcconfig "$CONFIG" -configuration Release -destination 'generic/platform=iOS' \
  -archivePath "$ARCHIVE" archive CODE_SIGNING_ALLOWED=NO \
  CURRENT_PROJECT_VERSION="$BUILD_NUMBER"

# Export retains these push and app group entitlements and replaces the
# ad-hoc signatures with Apple's distribution signature, as in the upstream
# TestFlight workflow. The extension shares the app group (the sessions Live
# Activity's wallpaper); nested code is signed before the app.
APP="$ARCHIVE/Products/Applications/Zeron.app"
codesign --force --sign - --entitlements apps/ios/ZeronLiveActivity.clemann.entitlements \
  "$APP/PlugIns/ZeronLiveActivity.appex"
codesign --force --sign - --entitlements apps/ios/Fork.clemann.entitlements "$APP"

python3 - "$APP/Info.plist" <<'PY'
import plistlib
import sys
with open(sys.argv[1], "rb") as file:
    info = plistlib.load(file)
expected = {
    "CFBundleIdentifier": "app.clemann.zeron",
    "ZeronEdgeURL": "https://zeron.clemann.app",
    "ZeronAuthAPIBase": "https://zeron.clemann.app",
    "ZeronAuthClientID": "zeron-self",
    "ZeronAppGroup": "group.app.clemann.zeron",
}
for key, value in expected.items():
    if info.get(key) != value:
        raise SystemExit(f"Refusing fork release: unexpected {key}: {info.get(key)!r}")
print(f"Verified fork archive, build {info['CFBundleVersion']}")
PY

if [[ "$ACTION" == archive ]]; then
  echo "Archive ready: $ARCHIVE"
  echo "Upload with BUILD_NUMBER=$BUILD_NUMBER $0 upload, or open the archive in Xcode."
  exit 0
fi

TEAM_ID="$(awk -F ' = ' '/^DEVELOPMENT_TEAM = / {print $2}' "$CONFIG")"
python3 - "$OUT/ExportOptions.plist" "$TEAM_ID" <<'PY'
import plistlib
import sys
with open(sys.argv[1], "wb") as file:
    plistlib.dump({
        "method": "app-store-connect",
        "destination": "upload",
        "signingStyle": "automatic",
        "teamID": sys.argv[2],
        "manageAppVersionAndBuildNumber": False,
        "testFlightInternalTestingOnly": True,
    }, file)
PY

AUTH=()
LOCAL_APPLE_CONFIG="${ZERON_APPLE_CONFIG:-$HOME/.zeron-selfhost/apple/zeron-testflight.json}"
if [[ -f "$LOCAL_APPLE_CONFIG" && -z "${AC_KEY_PATH:-}" ]]; then
  APPLE_VALUES="$(python3 - "$LOCAL_APPLE_CONFIG" <<'PY'
import json
import sys
with open(sys.argv[1]) as file:
    config = json.load(file)
for key in ["key_path", "key_id", "issuer_id"]:
    value = config[key]
    if not isinstance(value, str) or not value or "\n" in value:
        raise SystemExit(f"Invalid Apple upload setting: {key}")
    print(value)
PY
)"
  {
    IFS= read -r AC_KEY_PATH
    IFS= read -r AC_API_KEY_ID
    IFS= read -r AC_API_ISSUER_ID
  } <<< "$APPLE_VALUES"
fi
if [[ -n "${AC_KEY_PATH:-}${AC_API_KEY_ID:-}${AC_API_ISSUER_ID:-}" ]]; then
  : "${AC_KEY_PATH:?Set the path to your App Store Connect .p8 key}"
  : "${AC_API_KEY_ID:?Set the App Store Connect key ID}"
  : "${AC_API_ISSUER_ID:?Set the App Store Connect issuer ID}"
  AUTH=(-authenticationKeyPath "$AC_KEY_PATH" -authenticationKeyID "$AC_API_KEY_ID" \
    -authenticationKeyIssuerID "$AC_API_ISSUER_ID")
fi
xcodebuild -exportArchive -archivePath "$ARCHIVE" \
  -exportPath "$OUT/export" -exportOptionsPlist "$OUT/ExportOptions.plist" \
  -allowProvisioningUpdates ${AUTH[@]+"${AUTH[@]}"}
