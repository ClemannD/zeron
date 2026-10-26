# Personal iPhone builds through TestFlight

The fork uses `apps/ios/Fork.clemann.xcconfig` when archiving:

| Setting | Value |
| --- | --- |
| Apple team | `7C4DAG233S` (Dylan Clemann's local development certificate) |
| App bundle ID | `app.clemann.zeron` |
| Live Activity extension ID | `app.clemann.zeron.LiveActivity` |
| App Group (app + extension) | `group.app.clemann.zeron` |
| Sync and sign-in API | `https://zeron.clemann.app` |
| Auth client | `zeron-self` |

Normal upstream builds retain their original IDs and WorkOS settings. The
iPhone reads its endpoints from the built app's Info.plist; its Rust client
receives that edge URL explicitly. Android still uses upstream's defaults.

## One-time Apple account setup

1. Sign in to [Apple Developer](https://developer.apple.com/account/) and
   [App Store Connect](https://appstoreconnect.apple.com/). Confirm the team
   above is your paid membership's team; update the xcconfig if needed.
2. In Certificates, Identifiers & Profiles, register two explicit App IDs:
   `app.clemann.zeron` (description: Zeron Clemann; enable Push Notifications)
   and `app.clemann.zeron.LiveActivity` (description: Zeron Live Activity).
   Register the App Group `group.app.clemann.zeron`, then enable App Groups
   on both App IDs with that group selected. The sessions Live Activity reads
   the app's wallpaper from it. The export's automatic signing may do this
   itself; if an upload fails on provisioning, do it by hand.
3. In App Store Connect → Apps → + → New App, create an iOS app with:
   name `Zeron Clemann`, primary language English (U.S.), bundle ID
   `app.clemann.zeron`, and SKU `zeron-clemann-ios`. The name can differ if
   Apple reports it is unavailable. A public App Store release is not needed.
4. For automated uploads, open Users and Access → Integrations → App Store
   Connect API → Team Keys. Request API access if Apple requires it. Generate
   a team key named `Zeron TestFlight` with Admin access for automatic
   distribution signing/provisioning. Download its `.p8` file once and keep
   it outside the repo. Record the Key ID and Issuer ID. An individual API key
   is not interchangeable with the team key this workflow uses.

Apple documents [creating an app record](https://developer.apple.com/help/app-store-connect/create-an-app-record/add-a-new-app/)
and [creating team API keys](https://developer.apple.com/help/app-store-connect/get-started/app-store-connect-api/).

## Archive and upload from this Mac

Requires Xcode 26+, Rust, and `aarch64-apple-ios` in rustup. The first build
compiles Rust on this Mac; the phone installs only the finished app.

```sh
scripts/ios/archive-fork.sh
```

The output is `target/ios-fork/Zeron.xcarchive`. Its bundle ID and self-hosted
endpoints are checked before it can be uploaded. The archive is ad-hoc signed
to carry push entitlements; it is not an installable, distribution-signed app
until Xcode exports it.

To upload, sign into your developer account in Xcode → Settings → Accounts,
then run with a build number greater than all previously uploaded builds:

```sh
BUILD_NUMBER="${NEXT_BUILD_NUMBER:?Set from App Store Connect}" scripts/ios/archive-fork.sh upload
```

For subsequent releases, query App Store Connect for the existing app
(`app.clemann.zeron`, app ID `6826902828`), follow all build-list pages, and
set `NEXT_BUILD_NUMBER` above the highest uploaded build number. The script
defaults to `1`; it does not allocate the next number automatically. Do not
upload concurrently with another local or Actions release.

This Mac also has a private upload config at
`~/.zeron-selfhost/apple/zeron-testflight.json`, containing `key_path`, `key_id`
and `issuer_id`. The script loads it automatically; `ZERON_APPLE_CONFIG` can
select another local file. The `.p8` remains outside the repo.

Alternatively supply `AC_KEY_PATH` (the local `.p8` path), `AC_API_KEY_ID` and
`AC_API_ISSUER_ID` in the environment. Do not put key contents in shell
arguments, source files, or chat. Xcode signs the export with Apple's
distribution certificate and uploads it as an internal TestFlight build.

## Automated updates

Set these repository Actions secrets on `ClemannD/zeron`:

- `AC_API_KEY_P8`: contents of the downloaded team `.p8` file.
- `AC_API_KEY_ID`: its Key ID.
- `AC_API_ISSUER_ID`: its Issuer ID.

For example, `gh secret set AC_API_KEY_P8 --repo ClemannD/zeron < /path/to/key.p8`
reads the key without printing it. The other two can be entered at `gh secret
set` prompts. Do not add `CLOUDFLARE_API_TOKEN`: upstream's deployment workflow
must remain inert.

Once the setup changes are pushed, run **TestFlight (Clemann)** manually in
GitHub Actions. It selects the next build number from App Store Connect,
builds on macOS 26, verifies the fork endpoints, uploads to Apple, and removes
the temporary private key. The upstream TestFlight workflow is separate.
Check that the fork workflow exists on the remote ref before dispatching it;
a local workflow file alone is not available in Actions.

After Apple processes the first build, open the app's TestFlight tab, create
an internal testing group, and add your App Store Connect user and the build.
Enable automatic distribution for that internal group if desired. Install
TestFlight on the iPhone and accept the invitation there. Builds expire after
90 days; rerun the workflow to upload a refreshed build.

The app and internal group **Personal Devices** have already been created
for this fork. Normal updates reuse them and the existing TestFlight install;
no new invitation is needed. After each upload, verify Apple has finished
processing that build and the group has access. A processing-complete email
alone does not establish testing access.

Agents releasing features or merged changes should follow
[release-fork](../.claude/skills/release-fork/SKILL.md) for the complete
Mac/iPhone/backend workflow.

## Connect to the home Mac

Sign into the same self-hosted account/workspace on both devices. Use the
existing passkey, or issue an invite for the existing email to add a passkey
to that same account (see `docs/self-host.md`). Keep the home engine online
and the Mac awake. Select the home Mac as the host for sessions.

The iPhone target currently requires iOS 26 or later.

## Optional session push notifications

Controlling sessions works without APNs server credentials. For notifications,
create an APNs key for this Apple team, set `APNS_KEY_P8` and `APNS_KEY_ID` as
edge secrets, and set `APNS_TEAM_ID=7C4DAG233S` and
`APNS_TOPIC=app.clemann.zeron` in `edge/wrangler.clemann.jsonc`. Always use
`-c wrangler.clemann.jsonc` for Wrangler commands. The shared edge defaults
belong to upstream and must not be used with this fork's APNs key.

## Sessions Live Activity

The Lock Screen and Dynamic Island show the sessions this iPhone sent a
message to (a new session, a follow-up, or an answer). The edge drives it
(`edge/src/live-activity.ts`): the phone registers its ActivityKit tokens and
reports each send to `/registry/:orgId/live-activity`. The edge starts,
updates, and ends the activity over APNs, using the same `APNS_*` settings as
notifications. Pushes go to the `app.clemann.zeron.push-type.liveactivity`
topic; no extra key or secret is needed.

A session that needs input, finishes, or fails while others still run chimes
on the activity with the desktop's sound, and the plain notification is
skipped. The last session finishing ends the activity and leaves the alert to
the plain notification. `/registry/:orgId/stats` lists the registrations,
watched sessions, and recent sends under `liveActivity`.

The fork's archive signs the app and extension with
`apps/ios/Fork.clemann.entitlements` and
`apps/ios/ZeronLiveActivity.clemann.entitlements` (the App Group above).
Builds without the group still show the activity, on the system's material
instead of the wallpaper. On a debug build, `-sessions-activity-preview
[one|several|done]` starts a local activity with sample sessions.
