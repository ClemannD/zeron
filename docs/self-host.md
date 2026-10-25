# Self-hosted control plane (zeron.clemann.app)

Private Mac app releases use a separate authenticated R2 feed. See
[Mac private updates](macos-private-updates.md) for publishing and bootstrap
installation; these artifacts never go in the public `zeron-releases` bucket.

This fork runs its own edge and vault on the clemann.app Cloudflare account
(`27f9550dae9ea2ae400dff7b77cc052f`), with the edge as its own identity
provider (`AUTH_MODE=self`, `edge/src/self-auth/`): passkey sign-in instead of
WorkOS. Upstream's configs (`wrangler.jsonc`) are untouched; ours are
`edge/wrangler.clemann.jsonc` and `edge/vault/wrangler.clemann.jsonc`.

## Deploy

```sh
cd edge/vault && npx wrangler deploy -c wrangler.clemann.jsonc   # vault first
cd edge && npx wrangler deploy -c wrangler.clemann.jsonc
```

Every wrangler command needs `-c wrangler.clemann.jsonc`; without it wrangler
reads upstream's config and targets upstream's account.

Secrets were generated into `~/.zeron-selfhost/secrets.json` (0600). Move it
to a password manager; `VAULT_KEK` in particular is unrecoverable. Set one with
`npx wrangler secret put NAME -c wrangler.clemann.jsonc`.

| Worker | Secrets |
| --- | --- |
| zeron-vault | `VAULT_KEK` (optional `GITHUB_APP_*`) |
| zeron-edge | `AUTH_JWT_PRIVATE_KEY`, `AUTH_SETUP_CODE`, `ADMIN_TOKEN`, `RUNNER_JWT_PRIVATE_KEY`, `RUNNER_JWT_PUBLIC_KEY` (optional `BOAT_API_KEY`, `CLOUD_ACCESS` for Cloud sessions) |

## Sign-in

Engines open `{ZERON_WORKOS_API_BASE}/user_management/authorize`, which is the
edge's own passkey page. `scripts/install-fork-macos.sh` bakes the three env
vars into the app bundle:

```
ZERON_EDGE_URL=https://zeron.clemann.app
ZERON_WORKOS_API_BASE=https://zeron.clemann.app
ZERON_WORKOS_CLIENT_ID=zeron-self
```

A headless engine needs the same three in its environment (`zeron daemon
install` captures them).

- **First account:** on the sign-in page, "Set up a passkey" with the
  `AUTH_SETUP_CODE`. The code stops working once any account exists.
- **More passkeys / more people:** mint an invite and open its URL on the new
  device:
  ```sh
  curl -X POST https://zeron.clemann.app/admin/auth/invites \
    -H "authorization: Bearer $ADMIN_TOKEN" -d '{"email":"you@example.com"}'
  ```
  An invite for an existing email adds a passkey to that account. Pass
  `"orgId"` to add a new person to an existing workspace.
- **Accounts:** `GET /admin/auth/users`. **Sign someone out everywhere:**
  `POST /admin/auth/revoke {"userId": "..."}` (access tokens still live up to
  10 minutes).

iPhone fork archives use `apps/ios/Fork.clemann.xcconfig` to set this edge,
the auth API base, and `zeron-self` in the signed bundle. See
[the TestFlight setup](ios-testflight.md). Normal iOS builds and Android
still use upstream's defaults (`crates/client/src/auth.rs`).

## Upstream merges

When upstream changes `edge/wrangler.jsonc` (new Durable Objects, migrations,
Workflows, bindings), mirror the change in `wrangler.clemann.jsonc`. Append
new migration tags after ours (`self-auth-v1`).
