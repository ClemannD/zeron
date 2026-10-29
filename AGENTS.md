# Agent notes for this fork

This is a personal fork of `zeronsh/zeron` (`upstream`; `origin` is
`ClemannD/zeron`). It runs **its own backend** on the owner's own accounts. It
does not use upstream's infrastructure (edge.zeron.sh, WorkOS, upstream's
Cloudflare account). Read `docs/self-host.md` before touching `edge/`,
deploys, auth, or the app's edge settings.

## The self-hosted backend

| Piece | Where |
| --- | --- |
| Edge Worker `zeron-edge` (sync, relays, R2, Cloud control plane, **auth**) | https://zeron.clemann.app |
| Vault Worker `zeron-vault` (service binding only, no URL) | same account |
| Cloudflare account | `27f9550dae9ea2ae400dff7b77cc052f` |
| DNS zone | `clemann.app`, on Cloudflare (registered via DigitalOcean) |
| R2 buckets | `zeron-blobs`, `zeron-releases` |
| Private Mac releases | `zeron-private-releases`, served only to the configured owner at `/fork-updates/*` |
| Secrets (local copy) | `~/.zeron-selfhost/secrets.json`. Never print, commit, or regenerate them. A new `VAULT_KEK` destroys every stored credential, and a new `AUTH_JWT_PRIVATE_KEY` signs everyone out. |

**Auth is our own, not WorkOS.** `AUTH_MODE=self` makes the edge its own
identity provider (`edge/src/self-auth/`):

- **Sign-in:** passkeys. The sign-in page is at `/user_management/authorize`, and the client id is `zeron-self`.
- **Accounts:** sign-up is closed. The first account uses `AUTH_SETUP_CODE`, which is refused once any user exists. Everyone after that needs an invite from `/admin/auth/*` with `ADMIN_TOKEN`.
- **Tokens:** our own ES256 JWTs. `/auth/exchange`, `/auth/refresh` and `/auth/orgs` keep their WorkOS-era wire shapes, so the Rust engine needs no changes.

Keep the WorkOS code paths working (upstream uses them), but don't send this
fork's traffic to WorkOS.

## Rules

- **Always pass `-c wrangler.clemann.jsonc` to wrangler** (in `edge/` or
  `edge/vault/`): deploy, secrets, r2, tail, everything. Without it, wrangler
  reads upstream's `wrangler.jsonc` and targets **upstream's** account
  (`b57dde68…`). Deploy the vault before the edge:
  ```sh
  cd edge/vault && npx wrangler deploy -c wrangler.clemann.jsonc
  cd edge && npx wrangler deploy -c wrangler.clemann.jsonc
  ```
- **Don't edit upstream's `edge/wrangler.jsonc` or `edge/vault/wrangler.jsonc`
  for our deployment.** Our settings live only in the `*.clemann.jsonc` files,
  so upstream merges stay conflict-free.
- **On every upstream merge, diff `edge/wrangler.jsonc` and
  `edge/vault/wrangler.jsonc`.** Mirror new Durable Objects, migrations,
  Workflows, bindings, crons and vars into the `*.clemann.jsonc` files by hand.
  Append new migration tags **after** ours (`self-auth-v1`); never reorder or
  rename existing tags. Leave out upstream's Stripe vars, staging env and
  zeron.sh routes. Then run `npm test` in `edge/` and redeploy.
- **Shared skills live in `.agents/skills/`.** Create and edit every repo
  skill there so all agents can discover it. `.claude/skills` is a tracked
  relative directory symlink to `../.agents/skills`; do not keep separate
  copies or add per-skill links. Git tracks the canonical skills and this
  symlink, while other local `.agents/` and `.claude/` state stays ignored.
  Preserve this layout and both skill exceptions if an upstream merge
  touches `.gitignore`.
- **Don't push to `main` expecting CI to deploy.** `.github/workflows/deploy.yml`
  is upstream's and targets upstream's configs. It stays inert while the fork
  has no `CLOUDFLARE_API_TOKEN` secret. Don't add one.
- **The macOS app is built with `scripts/install-fork-macos.sh`.** It bakes
  `ZERON_EDGE_URL`, `ZERON_WORKOS_API_BASE` (both `https://zeron.clemann.app`)
  and `ZERON_WORKOS_CLIENT_ID=zeron-self` into the bundle, and turns
  auto-update off. Headless engines need the same three env vars.
- **Distributable Mac releases use `scripts/release-fork-macos.sh publish`.**
  This builds the app plus engine, signs and notarizes them, and publishes to
  the private bucket. `ZERON_FORK_BUILD` enables the authenticated private
  updater and a distinct build identity. See `docs/macos-private-updates.md`.
  Never publish fork artifacts into the public `RELEASES` binding.
- **iPhone fork builds use `scripts/ios/archive-fork.sh`.** It passes
  `apps/ios/Fork.clemann.xcconfig` (our Apple team, bundle IDs, edge and auth
  settings). See `docs/ios-testflight.md`. Normal iOS builds and Android
  still use upstream defaults; keep those defaults working.
- **Cloud sessions are off:** there's no `BOAT_API_KEY`, and Stripe billing
  isn't configured.

## Testing the edge

`cd edge && npm run typecheck && npm test` (unit + workerd tiers).
`test/workerd/self-auth.workerd.test.ts` covers self-auth end to end with a
software passkey (`test/workerd/passkey-fake.ts`).

## Releasing changes to personal devices

Use [the release-fork skill](.agents/skills/release-fork/SKILL.md) when asked
to release, ship, publish an update, or update the installed apps after a
feature or merge. It covers private Mac updates, iPhone TestFlight, backend
deploys, validation, and version allocation. Read that file directly if the
agent's skill catalog does not list it; `.agents/skills/` is the tracked source.

Implementing a feature or merging code alone does not publish an app.
For a release request without a specified device, release the affected Mac
and iPhone clients; shared Rust/client changes can affect both. A backend-only
change needs a backend deploy, not new app builds. Report which app releases
were published and any remaining Apple processing. Do not replace or restart the
running home Mac app merely to publish an update for other devices.

For a changelog-first upstream review followed by a confirmed merge and
release, use
[upstream-changelog-and-pull](.agents/skills/upstream-changelog-and-pull/SKILL.md).
For directly authorized upstream syncs, use
[update-from-upstream](.agents/skills/update-from-upstream/SKILL.md), then
hand off to `release-fork` if shipping the merged changes was requested.
Pushing `main` alone does not publish either app.
