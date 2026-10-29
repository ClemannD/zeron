---
name: release-fork
description: Release this personal Zeron fork to Macs through its private update feed, to iPhone through TestFlight, or deploy its self-hosted backend after feature changes or merges.
---

# Release this fork

Run from the repository root on the signing Mac. Read `AGENTS.md` first.
This workflow ships an authorized release; ordinary feature implementation
and code merges do not implicitly request publication. Honor the requested
platforms. When a release request leaves platforms unspecified, inspect the
changes and release affected clients: desktop UI changes need Mac, iOS changes
need iPhone, shared Rust/client changes may need both. Backend-only changes
need a deploy. Documentation-only changes need no app release.

## Establish the source and prerequisites

- Inspect `git status --short`, the diff, and the commit/range being released.
  Prefer a committed source revision for reproducibility. Do not discard,
  stash, commit, or publish unrelated work silently. The Mac script builds the
  current worktree and records its commit and dirty state; if unrelated edits
  would enter the binary, isolate the intended revision or resolve the scope
  with the user before publishing. Do not run concurrent builds in the same
  checkout while packaging.
- Run checks appropriate to the changes, using existing tests. For backend
  changes read `docs/self-host.md` and run `npm run typecheck && npm test` in
  `edge/`. For updater changes also exercise the private updater and remote
  peer access tests; details are in `docs/macos-private-updates.md` and source.
  Preserve normal upstream builds and WorkOS paths while testing fork behavior.
- Apple credentials live in `~/.zeron-selfhost/apple/zeron-testflight.json`
  (`key_path`, `key_id`, `issuer_id`); the referenced `.p8` and Developer ID
  private key stay outside the repo. Never print keys or bearer tokens.
  Existing setup normally needs no new certificate, app record, invitation,
  backend secret, or signing key. If credentials are unavailable, report the
  specific prerequisite; do not regenerate them or repeat failed uploads.
- Use CLI/API tooling. Do not use Yab. Website-only account actions belong
  to the user; normal releases use the existing account configuration.

## Backend, when required

For an upstream merge, inspect changes to both upstream Wrangler configs
against the pre-merge revision and mirror required bindings, migrations,
Workflows, crons, and vars into the fork configs as directed by `AGENTS.md`.
Run edge checks and redeploy after an upstream merge. Otherwise deploy when
backend code/config changed, arranging compatible backend changes before
clients that need them. For an incompatible protocol change, resolve the
upgrade sequence before publishing.

Deploy the vault first, then the edge, each from its own directory:

```sh
(cd edge/vault && npx wrangler deploy -c wrangler.clemann.jsonc)
(cd edge && npx wrangler deploy -c wrangler.clemann.jsonc)
```

Every Wrangler command must use the fork config. Never target upstream's
account or edit its configs for our deployment. Do not add the GitHub
`CLOUDFLARE_API_TOKEN` secret or enable upstream deployment/release workflows.

## Mac: signed private in-app release

Read `docs/macos-private-updates.md`, then run:

```sh
scripts/release-fork-macos.sh publish
```

The script allocates a unique fork build number, builds the app **and engine**,
signs with team `7C4DAG233S`, notarizes and staples, checks fork settings,
creates ZIP/tar artifacts and checksums, uploads both artifacts to
`zeron-private-releases`, then publishes the manifest last. Its default
`build` action prepares artifacts without publishing. Do not bump the Cargo
workspace version just for a fork release: the update identity is
`<upstream version>.<fork build>`. Failed attempts consume build numbers.
Do not bypass its release lock; inspect any stale lock before removing it.

Do not substitute the development installer or upstream packager. Never
publish private artifacts to the public `zeron-releases`/`RELEASES` bucket.
Keep the signed app in `target/package/`; publishing does not install it over
the running `/Applications/Zeron.app` or interrupt local sessions.

Confirm the script finished publication, record the version from
`target/package/manifest.json`, and verify the remote manifest matches it
using authenticated access or a private R2 read with the fork config. Keep
tokens out of logs. A plain anonymous request to
`https://zeron.clemann.app/fork-updates/manifest.json` must be refused.
If upload fails, inspect remote state before retrying; never expose a
manifest until its referenced artifacts are present.

Existing bootstrapped Macs use **Check for Updates…**, then **Restart to
update** when local work is finished. For a first install, provide the signed
ZIP path and the one-time engine restart instructions in the Mac guide.
Rollback instructions are there too; restoring a previously published
manifest alone will not make clients install a lower version.

## iPhone: internal TestFlight release

Read `docs/ios-testflight.md`. Use the existing **Zeron Clemann** app, bundle
`app.clemann.zeron`, Apple app ID `6826902828`, team `7C4DAG233S`, and internal
group **Personal Devices**. Do not recreate account setup for an update.

For a local upload, query App Store Connect's builds for this app using the
saved team API credentials. Follow pagination and choose an integer greater
than every existing uploaded build number; serialize uploads to avoid number
collisions. The API code in `.github/workflows/testflight-clemann.yml` is a
starting point. Keep JWTs in memory. Set the queried number, then run:

```sh
BUILD_NUMBER="${NEXT_BUILD_NUMBER:?Set from App Store Connect}" scripts/ios/archive-fork.sh upload
```

The script's default number is `1`, so always supply the next number on
subsequent releases. It builds the fork archive, checks IDs/endpoints, and
uses Xcode to sign/export/upload. A local archive alone is not distributed.

Alternatively use **TestFlight (Clemann)** in GitHub Actions after verifying
`testflight-clemann.yml` exists on the remote release ref and the Apple
Actions secrets are configured. Dispatch that fork workflow and monitor its
result. Local presence of the workflow does not mean it is available remotely.
CI builds the selected committed ref; local builds include working-tree edits.

After uploading, check App Store Connect for this exact build's processing
and internal-testing availability, and confirm the existing group has access.
Upload success or a processing-complete email does not prove tester access.
If Apple is still processing when handing back, state that explicitly and
identify the build; do not claim the phone has the update. Investigate a
failed build before retrying with a new number. Routine updates use the
existing TestFlight installation and group, without a new invitation.

## Report the result

Report the source revision (and dirty state when applicable), Mac update
version and bootstrap ZIP path, iOS version/build and processing/testing
state, backend deployments if any, and checks performed. Include only the
platforms in scope. Distinguish published, uploaded/processing, and installed:
the user still chooses when to restart the Mac or install from TestFlight.
