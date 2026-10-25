# Private Mac updates

Release builds use the existing in-app updater with an authenticated feed:

- Feed: `https://zeron.clemann.app/fork-updates/manifest.json`
- R2: `zeron-private-releases`, separate from the public `RELEASES` bucket.
- Access: the owner user ID configured in `PRIVATE_RELEASES_USER_ID`; runner
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
engine after closing the app. Reopen Zeron using the existing self-hosted account.

The initial engine restart matters: an older engine does not implement the
new authenticated update RPC and may have the same upstream version number
as the bootstrap build. Subsequent private releases have distinct update
versions; the managed engine detects replacement and waits for no active
runs or open terminals before restarting into it.

## Normal updates

The app checks at launch and periodically, and **Check for Updates…** checks
immediately. Signing in or changing the engine connection also wakes the
checker. An available release downloads in the background and offers
**Restart to update**. A staged release also installs on normal app quit.

The UI obtains access tokens over authenticated local IPC from the engine,
which owns the rotating refresh token. The update client refreshes once on
HTTP 401. It sends credentials only to the fixed fork feed and never follows
private update redirects or falls back to upstream/public releases.

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

Updater tests cover credential destination restrictions, refresh-on-401,
redirect handling, mandatory checksums, interrupted downloads and staging.
Workerd tests cover self-auth JWTs, the owner allowlist, runner refusal,
HEAD requests, malformed paths, and isolation from the public release route.

The iPhone continues to update through TestFlight; see `ios-testflight.md`.
