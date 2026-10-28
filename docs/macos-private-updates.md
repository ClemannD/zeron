# Private Mac updates

Release builds use the existing in-app updater with an authenticated feed:

- Feed: `https://zeron.clemann.app/fork-updates/manifest.json`
- R2: `zeron-private-releases`, separate from the public `RELEASES` bucket.
- Access: a release-only credential bundled into signed Mac releases allows
  local-only Macs to update while signed out. It reads only `/fork-updates/*`;
  it grants no account, chat, vault, device, or sync access. The owner user ID
  in `PRIVATE_RELEASES_USER_ID` remains accepted for older releases; runner
  tokens and other users are refused. Responses are `private, no-store`.
- Signing: Developer ID Application for Apple team `7C4DAG233S`, bundle ID
  `sh.zeron.app`, and a separately signed `sh.zeron.app.engine` executable.

The edge route and bucket are configured only in `edge/wrangler.clemann.jsonc`.
Always pass `-c wrangler.clemann.jsonc` to Wrangler; the upstream deployment
configs and public releases are independent.

## Signing setup on the home Mac

Apple requires the Account Holder to create a Developer ID Application
certificate. A CSR and its private key are prepared outside the repository:

```
~/.zeron-selfhost/apple/developer-id.csr
~/.zeron-selfhost/apple/developer-id.key
```

Upload the CSR in Apple Developer → Certificates → + → Developer ID
Application. Download the `.cer` to `~/.zeron-selfhost/apple/developer-id.cer`.
Select the **G2** intermediary if Apple asks which authority to use.
The downloaded certificate must match the prepared private key, and both
must be imported into the login keychain for signing. Never replace an
existing signing key just to renew a certificate.

```sh
scripts/setup-fork-macos-signing.sh
```

The helper validates the team and key match before importing. macOS may ask
you to authorize codesign's first use of the imported key.

The notarization API credentials are the same saved team credentials used
for TestFlight, in `~/.zeron-selfhost/apple/zeron-testflight.json`. Keys and
credentials remain outside the repo.

## Build and publish

One-time update credential setup on the signing Mac:

```sh
python3 - <<'PY'
import os, pathlib, secrets
path = pathlib.Path.home() / '.zeron-selfhost/macos-update-token'
path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
if not path.exists():
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w') as file:
        file.write(secrets.token_hex(32) + '\n')
PY
(cd edge && npx wrangler secret put PRIVATE_RELEASES_TOKEN -c wrangler.clemann.jsonc < "$HOME/.zeron-selfhost/macos-update-token")
```

Keep this file outside the repository. The release script embeds its value
into the app and engine at compile time as `ZERON_FORK_UPDATE_TOKEN`; it is
not an account token or a signing key. Anyone holding the app can extract
this credential and read release files, but cannot use it for sync. Do not
rotate it casually: installed local-only Macs would need a new installer.
Neither the value nor account/signing credentials belong in logs or Git.

From the repository root on this Mac:

```sh
scripts/release-fork-macos.sh publish
```

This allocates the next fork build number, builds both Mac executables,
bakes the fork endpoints and update settings into the app, signs with our
Developer ID, submits to Apple notarization, and staples the accepted ticket.
It verifies the app before producing:

- `target/package/zeron-<update-version>-macos-arm64.zip` for first installs.
- `target/package/zeron-<update-version>-macos-arm64-app.tar.gz` for in-app updates.
- `target/package/manifest.json`, containing the update version and SHA-256s.

Both archives contain the complete app and stapled ticket. The packages are
uploaded first, then the manifest, so clients never discover a release whose
package is still uploading. `build` instead of `publish` stops after preparing
the artifacts. `BUILD_NUMBER` can supply an explicit number greater than the
last allocated one. Allocation is recorded in
`~/.zeron-selfhost/macos-build-number`; failed attempts consume a number too.

The update identity is `<upstream version>.<fork build>` (for example,
`0.2.124.7`). Normal app metadata retains the three-part upstream version,
with the fork number in `CFBundleVersion` and the full update identity in
`ZeronUpdateVersion`. Both executables report the full update identity.
This allows fork-only changes to update without editing upstream's Cargo
workspace version. Upstream builds without `ZERON_FORK_BUILD` keep their
original behavior.

`scripts/install-fork-macos.sh` without a fork build number still makes a
development build with automatic updates disabled. The release script builds
only; it does not replace the running app on this home Mac.

## One-time installation on each Mac

Transfer the first notarized ZIP to each Mac, extract it, and replace
`/Applications/Zeron.app`. Quit Zeron before replacing it and restart that
Mac once after installation, once local agent runs have finished. This clears
the old background engine. Alternatively, explicitly quit the old background
engine after closing the app. Reopen Zeron; keep local-only Macs signed out,
and sign in only on Macs where you want sync.

The initial engine restart matters: an older engine can outlive the app it
came with and may have the same upstream version number as the bootstrap
build. Subsequent private releases have distinct update
versions; the managed engine detects replacement and waits for no active
runs or open terminals before restarting into it.

## Normal updates

The app checks at launch and periodically, and **Check for Updates…** checks
immediately. Signing in or changing the engine connection also wakes the
checker. An available release downloads in the background and offers
**Restart to update**. A staged release also installs on normal app quit.

Current signed releases check and download using the bundled release-only
credential. They work without a sync account, an engine auth session, or
registering the Mac with the backend. Remaining signed out keeps the Mac's
chats local; update requests only read the feed and release package.

Older private releases obtain account access tokens over authenticated local
IPC from the engine and refresh once on HTTP 401. A signed-out Mac on one of
these builds needs the current signed ZIP installed once; after that it can
update while signed out. Both paths send credentials only to the fixed fork
feed and never follow private update redirects or fall back to public releases.

Before executing any staged binary, the updater requires its manifest's
checksum, verifies Developer ID signatures for both executables against our
team and identifiers, and assesses Apple's notarization policy. Those checks
run again before replacement. Missing checksums, unexpected signatures, and
damaged downloads fail without replacing the installed app.

The app bundle is replaced without moving settings or session data. The
previous bundle remains at `/Applications/.Zeron.app.previous`. To roll back,
quit the app and engine and restore that bundle as `Zeron.app`. A failed
bundle swap restores the original automatically; failed-launch rollback is
manual.

## Backend changes and validation

After changing the edge, run `npm run typecheck && npm test` in `edge/`.
Deploy the vault first, then the edge, using the fork configs. The private
release route does not require new Durable Object migrations or new auth keys.

Updater tests cover signed-out capability access, credential destination restrictions, refresh-on-401,
redirect handling, mandatory checksums, interrupted downloads and staging.
Workerd tests cover self-auth JWTs, the owner allowlist, runner refusal,
HEAD requests, malformed paths, and isolation from the public release route
and from sync, vault, and account APIs.

The iPhone continues to update through TestFlight; see `ios-testflight.md`.
