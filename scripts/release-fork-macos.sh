#!/usr/bin/env bash
# Build a signed, notarized private-channel app. Add "publish" to upload it.
# Credentials stay in ~/.zeron-selfhost/apple. Only this Mac needs Rust/Xcode.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ACTION="${1:-build}"
case "$ACTION" in build|publish) ;; *) echo "usage: $0 [build|publish]" >&2; exit 2 ;; esac
cd "$ROOT"

# Packaging uses one working app bundle. Serialize publishers on this Mac so
# two builds cannot sign or notarize each other's partially replaced bundle.
RELEASE_LOCK="$HOME/.zeron-selfhost/macos-release.lock"
if ! mkdir -m 700 "$RELEASE_LOCK" 2>/dev/null; then
  echo "Another Mac release is running (lock: $RELEASE_LOCK)." >&2
  exit 2
fi
trap 'rmdir "$RELEASE_LOCK" 2>/dev/null || true' EXIT

if [[ -z "${CODESIGN_IDENTITY:-}" ]]; then
  CODESIGN_IDENTITY="$(security find-identity -v -p codesigning | sed -n 's/.*"\(Developer ID Application: .* (7C4DAG233S)\)".*/\1/p' | head -1)"
fi
if [[ -z "$CODESIGN_IDENTITY" ]]; then
  echo "Install this team's Developer ID Application certificate first (docs/macos-private-updates.md)." >&2
  exit 2
fi
export CODESIGN_IDENTITY
APPLE_CONFIG="${ZERON_APPLE_CONFIG:-$HOME/.zeron-selfhost/apple/zeron-testflight.json}"
APPLE_VALUES="$(python3 - "$APPLE_CONFIG" <<'PY'
import json, sys
with open(sys.argv[1]) as file:
    config = json.load(file)
for key in ['key_path', 'key_id', 'issuer_id']:
    value = config[key]
    if not isinstance(value, str) or not value or '\n' in value:
        raise SystemExit(f'Invalid Apple setting: {key}')
    print(value)
PY
)"
{ IFS= read -r NOTARY_KEY_PATH; IFS= read -r NOTARY_KEY_ID; IFS= read -r NOTARY_ISSUER_ID; } <<< "$APPLE_VALUES"

# Allocate even failed build numbers: a release number is never reused.
ZERON_FORK_BUILD="$(python3 - "${BUILD_NUMBER:-}" "$HOME/.zeron-selfhost/macos-build-number" <<'PY'
import fcntl, pathlib, sys
override, filename = sys.argv[1:]
path = pathlib.Path(filename)
path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
with path.open('a+') as file:
    fcntl.flock(file, fcntl.LOCK_EX)
    file.seek(0)
    previous = int(file.read().strip() or '0')
    number = int(override) if override else previous + 1
    if number <= previous:
        raise SystemExit(f'Build number must be greater than {previous}')
    file.seek(0); file.truncate(); file.write(str(number) + '\n'); file.flush()
print(number)
PY
)"
export ZERON_FORK_BUILD
# An update-only capability keeps local-only Macs out of account/sync auth.
# Provision the same value as the edge's PRIVATE_RELEASES_TOKEN once (guide).
ZERON_FORK_UPDATE_TOKEN="$(python3 - "$HOME/.zeron-selfhost/macos-update-token" <<'PY'
import pathlib, re, sys
path = pathlib.Path(sys.argv[1])
if not path.is_file():
    raise SystemExit('Configure macos-update-token first; see docs/macos-private-updates.md')
token = path.read_text().strip()
if not re.fullmatch(r'[a-f0-9]{64}', token):
    raise SystemExit('Invalid macos-update-token: expected 32 random bytes as hex')
print(token)
PY
)"
export ZERON_FORK_UPDATE_TOKEN
ZERON_FORK_BUILD_ONLY=1 scripts/install-fork-macos.sh
unset ZERON_FORK_UPDATE_TOKEN
OUT="$ROOT/target/package"
APP="$OUT/Zeron.app"
VERSION="$(plutil -extract ZeronUpdateVersion raw -o - "$APP/Contents/Info.plist")"
ARCH="$(uname -m)"
python3 - "$APP/Contents/Info.plist" "$APP/Contents/MacOS/zeron" "$VERSION" <<'PY'
import plistlib, subprocess, sys
with open(sys.argv[1], 'rb') as file:
    info = plistlib.load(file)
expected = {'ZERON_EDGE_URL': 'https://zeron.clemann.app',
            'ZERON_WORKOS_API_BASE': 'https://zeron.clemann.app',
            'ZERON_WORKOS_CLIENT_ID': 'zeron-self',
            'ZERON_RELEASES_URL': 'https://zeron.clemann.app/fork-updates',
            'ZERON_AUTO_UPDATE': '1'}
for key, value in expected.items():
    if info.get('LSEnvironment', {}).get(key) != value:
        raise SystemExit(f'Refusing private release: unexpected {key}')
reported = subprocess.check_output([sys.argv[2], '--version'], text=True).strip()
if reported != 'zeron ' + sys.argv[3]:
    raise SystemExit('Refusing private release: binary and manifest versions differ')
PY
ZIP="$OUT/zeron-$VERSION-macos-$ARCH.zip"
TARBALL="$OUT/zeron-$VERSION-macos-$ARCH-app.tar.gz"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP"
xcrun notarytool submit "$ZIP" --key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" \
  --issuer "$NOTARY_ISSUER_ID" --wait --output-format json > "$OUT/notary-$VERSION.json"
python3 - "$OUT/notary-$VERSION.json" <<'PY'
import json, sys
with open(sys.argv[1]) as file:
    result = json.load(file)
if result.get('status') != 'Accepted':
    raise SystemExit(f"Apple notarization failed: {result.get('status')} (submission {result.get('id')})")
print('Apple notarization accepted:', result['id'])
PY
xcrun stapler staple "$APP"
codesign --verify --deep --strict "$APP"
spctl --assess --type execute "$APP"
# Both artifacts carry the stapled ticket; the ZIP bootstraps older installs.
ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP"
tar -czf "$TARBALL" -C "$OUT" Zeron.app
python3 - "$VERSION" "$TARBALL" "$ZIP" "$OUT/manifest.json" <<'PY'
import datetime, hashlib, json, pathlib, subprocess, sys
version, tarball, archive, manifest = sys.argv[1:]
files = {}
for filename in [tarball, archive]:
    path = pathlib.Path(filename)
    files[path.name] = {'sha256': hashlib.file_digest(path.open('rb'), 'sha256').hexdigest()}
data = {'version': version, 'files': files,
        'commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
        'dirty': bool(subprocess.check_output(['git', 'status', '--porcelain'], text=True).strip()),
        'publishedAt': datetime.datetime.now(datetime.timezone.utc).isoformat()}
pathlib.Path(manifest).write_text(json.dumps(data, indent=2) + '\n')
print('Private release ready:', version)
PY
if [[ "$ACTION" == publish ]]; then
  # Refuse publication if local-only clients cannot use this credential.
  # Never follow redirects with a credential or print it on failure.
  python3 - "$HOME/.zeron-selfhost/macos-update-token" <<'PY'
import pathlib, sys, urllib.error, urllib.request
class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None
token = pathlib.Path(sys.argv[1]).read_text().strip()
request = urllib.request.Request('https://zeron.clemann.app/fork-updates/manifest.json',
    headers={'X-Zeron-Update-Token': token, 'User-Agent': 'zeron-release/1'})
try:
    with urllib.request.build_opener(NoRedirect()).open(request, timeout=30) as response:
        if response.status != 200:
            raise SystemExit('Private update credential check failed')
except urllib.error.HTTPError as error:
    # The first release may not have a manifest yet. Auth is checked before R2.
    if error.code != 404:
        raise SystemExit(f'Private update credential check failed (HTTP {error.code}); configure the edge before publishing')
PY
  cd "$ROOT/edge"
  # The public RELEASES binding is deliberately never used for fork artifacts.
  for artifact in "$TARBALL" "$ZIP"; do
    npx wrangler r2 object put "zeron-private-releases/$(basename "$artifact")" \
      --file "$artifact" --remote -c wrangler.clemann.jsonc
  done
  npx wrangler r2 object put zeron-private-releases/manifest.json \
    --file "$OUT/manifest.json" --content-type application/json --remote -c wrangler.clemann.jsonc
  echo "Published private Mac release $VERSION"
else
  echo "Artifacts: $ZIP and $TARBALL"
fi
