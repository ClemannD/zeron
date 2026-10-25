/**
 * Self-hosted auth (AUTH_MODE=self, src/self-auth/): the edge as its own
 * identity provider. A software passkey drives the real WebAuthn
 * verification; the engine-facing routes keep their WorkOS wire shapes.
 *
 * The directory is one DO (`auth1`) shared by every test in this file, so
 * the tests run in order: the first creates the first account.
 */
import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { authenticate } from "../../src/auth";
import { handleAuthRoute } from "../../src/auth-routes";
import type { Env } from "../../src/env";
import { allowedRedirect, handleSelfAuthRoute } from "../../src/self-auth/routes";
import { createFakePasskey, type FakePasskey } from "./passkey-fake";

const ORIGIN = "https://zeron.example.test";
const CLIENT_ID = "test-only";
const LOOPBACK = "http://127.0.0.1:27641/callback";
const selfEnv = { ...env, AUTH_MODE: "self", ADMIN_TOKEN: "test-admin-token" } as unknown as Env;

const request = (method: string, path: string, body?: unknown, headers: Record<string, string> = {}) =>
  new Request(`${ORIGIN}${path}`, {
    method,
    headers: { origin: ORIGIN, ...(body === undefined ? {} : { "content-type": "application/json" }), ...headers },
    ...(body === undefined ? {} : { body: JSON.stringify(body) })
  });

/** Route a request the way index.ts does (self-auth first, then /auth/*, then the bearer gate). */
const call = async (method: string, path: string, body?: unknown, headers?: Record<string, string>) => {
  const req = request(method, path, body, headers);
  const url = new URL(req.url);
  const res = (await handleSelfAuthRoute(req, selfEnv, url)) ?? (await handleAuthRoute(req, selfEnv, url));
  if (!res) throw new Error(`unrouted: ${method} ${path}`);
  return res;
};

const jsonOf = async <T>(res: Response) => (await res.json()) as T;

/** Register a passkey through the page's endpoints; returns the sign-in code. */
const register = async (pk: FakePasskey, email: string, code: string, redirectUri = LOOPBACK) => {
  const opts = await call("POST", "/auth/passkey/register/options", { email, name: "Ada Lovelace", code });
  if (opts.status !== 200) return { status: opts.status, error: (await jsonOf<{ error: string }>(opts)).error };
  const { challengeId, options } = await jsonOf<{ challengeId: string; options: never }>(opts);
  const response = await pk.register(options, ORIGIN);
  const verify = await call("POST", "/auth/passkey/register/verify", { challengeId, response, redirectUri, state: "st-1" });
  const body = await jsonOf<{ redirect?: string; error?: string }>(verify);
  return { status: verify.status, error: body.error, redirect: body.redirect };
};

const signIn = async (pk: FakePasskey, redirectUri = LOOPBACK) => {
  const { challengeId, options } = await jsonOf<{ challengeId: string; options: never }>(
    await call("POST", "/auth/passkey/login/options", {})
  );
  const response = await pk.assert(options, ORIGIN);
  const verify = await call("POST", "/auth/passkey/login/verify", { challengeId, response, redirectUri, state: "st-2" });
  return { status: verify.status, body: await jsonOf<{ redirect?: string; error?: string }>(verify) };
};

const codeOf = (redirect: string) => new URL(redirect).searchParams.get("code")!;

type Exchange = { user: { id: string; email: string; firstName: string | null }; accessToken: string; refreshToken: string };

const exchange = async (code: string) => call("POST", "/auth/exchange", { code });
const refresh = async (refreshToken: string, organizationId?: string) =>
  call("POST", "/auth/refresh", { refreshToken, ...(organizationId ? { organizationId } : {}) });

const claims = (jwt: string) => JSON.parse(atob(jwt.split(".")[1]!.replaceAll("-", "+").replaceAll("_", "/"))) as Record<string, unknown>;

const owner = await createFakePasskey();
let ownerSession: Exchange;

describe("self-hosted auth", () => {
  it("only accepts sign-in redirects to the engine, the app, or our own paste page", () => {
    expect(allowedRedirect("http://127.0.0.1:5000/callback", ORIGIN)).toBeDefined();
    expect(allowedRedirect("http://localhost:5000/callback", ORIGIN)).toBeDefined();
    expect(allowedRedirect("zeron://callback", ORIGIN)).toBeDefined();
    expect(allowedRedirect(`${ORIGIN}/auth/cli/callback`, ORIGIN)).toBeDefined();
    expect(allowedRedirect("https://evil.example/callback", ORIGIN)).toBeUndefined();
    expect(allowedRedirect("http://127.0.0.1.evil.example/callback", ORIGIN)).toBeUndefined();
    expect(allowedRedirect("http://user@127.0.0.1:5000/callback", ORIGIN)).toBeUndefined();
    expect(allowedRedirect("http://127.0.0.1:5000/other", ORIGIN)).toBeUndefined();
  });

  it("serves the sign-in page only for a valid authorize request, offering setup while empty", async () => {
    const good = await call(
      "GET",
      `/user_management/authorize?response_type=code&client_id=${CLIENT_ID}&redirect_uri=${encodeURIComponent(LOOPBACK)}&provider=authkit&state=s`
    );
    expect(good.status).toBe(200);
    const page = await good.text();
    expect(page).toContain("Set up a passkey");
    expect(page).toMatch(/<section id="signin" hidden>/);
    const bad = await call(
      "GET",
      `/user_management/authorize?response_type=code&client_id=${CLIENT_ID}&redirect_uri=${encodeURIComponent("https://evil.example/cb")}&state=s`
    );
    expect(bad.status).toBe(400);
  });

  it("refuses a wrong setup code, then creates the first account with the right one", async () => {
    const wrong = await register(owner, "ada@example.com", "not-the-code");
    expect(wrong.status).toBe(403);
    const made = await register(owner, "Ada@Example.com", "test-setup-code");
    expect(made.status, made.error).toBe(200);
    expect(made.redirect).toMatch(/^http:\/\/127\.0\.0\.1:27641\/callback\?code=.+&state=st-1$/);
    const res = await exchange(codeOf(made.redirect!));
    expect(res.status).toBe(200);
    ownerSession = await jsonOf<Exchange>(res);
    expect(ownerSession.user.email).toBe("ada@example.com");
    expect(ownerSession.user.firstName).toBe("Ada");
    // Lands in its own workspace: the token is already org-scoped.
    expect(typeof claims(ownerSession.accessToken).org_id).toBe("string");
  });

  it("refuses the setup code once an account exists", async () => {
    const again = await register(await createFakePasskey(), "eve@example.com", "test-setup-code");
    expect(again.status).toBe(403);
  });

  it("signs in with the passkey; a code works once", async () => {
    const { status, body } = await signIn(owner);
    expect(status).toBe(200);
    const code = codeOf(body.redirect!);
    expect((await exchange(code)).status).toBe(200);
    expect((await exchange(code)).status).toBe(401);
  });

  it("rejects an assertion made for another origin, or with a tampered signature", async () => {
    const options = async () =>
      jsonOf<{ challengeId: string; options: never }>(await call("POST", "/auth/passkey/login/options", {}));
    const phished = await options();
    const elsewhere = await owner.assert(phished.options, "https://evil.example");
    const a = await call("POST", "/auth/passkey/login/verify", { challengeId: phished.challengeId, response: elsewhere, redirectUri: LOOPBACK });
    expect(a.status).toBe(401);
    const forged = await options();
    const real = (await owner.assert(forged.options, ORIGIN)) as { response: { signature: string } };
    const sig = real.response.signature;
    real.response.signature = sig.slice(0, -4) + (sig.endsWith("AAAA") ? "BBBB" : "AAAA");
    const b = await call("POST", "/auth/passkey/login/verify", { challengeId: forged.challengeId, response: real, redirectUri: LOOPBACK });
    expect(b.status).toBe(401);
  });

  it("rejects an unknown passkey", async () => {
    const { status } = await signIn(await createFakePasskey());
    expect(status).toBe(401);
  });

  it("accepts its access tokens at the bearer gate with the WorkOS-shaped claims", async () => {
    const req = new Request(`${ORIGIN}/registry/x/rows`, { headers: { authorization: `Bearer ${ownerSession.accessToken}` } });
    const verified = await authenticate(selfEnv, req);
    expect(verified?.userId).toBe(ownerSession.user.id);
    expect(verified?.kind).toBe("user");
    expect(verified?.orgId).toBe(claims(ownerSession.accessToken).org_id);
    // A dev-mode style bearer means nothing here.
    const dev = await authenticate(selfEnv, new Request(`${ORIGIN}/x`, { headers: { authorization: `Bearer ${ownerSession.user.id}` } }));
    expect(dev).toBeUndefined();
  });

  it("lists and creates orgs, and scopes a refresh to a member org only", async () => {
    const auth = { authorization: `Bearer ${ownerSession.accessToken}` };
    const listed = await jsonOf<{ orgs: { organizationId: string; name: string }[] }>(await call("GET", "/auth/orgs", undefined, auth));
    expect(listed.orgs).toHaveLength(1);
    expect(listed.orgs[0]!.name).toBe("Ada's workspace");
    const created = await jsonOf<{ organizationId: string }>(await call("POST", "/auth/orgs", { name: "Side project" }, auth));
    const scoped = await refresh(ownerSession.refreshToken, created.organizationId);
    expect(scoped.status).toBe(200);
    const next = await jsonOf<{ accessToken: string; refreshToken: string }>(scoped);
    expect(claims(next.accessToken).org_id).toBe(created.organizationId);
    ownerSession = { ...ownerSession, ...next };
    const foreign = await refresh(ownerSession.refreshToken, "org_not_mine");
    expect(foreign.status).toBe(401);
  });

  it("rotates refresh tokens, tolerates a quick retry, and revokes the session on late reuse", async () => {
    const first = ownerSession.refreshToken;
    const a = await jsonOf<{ refreshToken: string }>(await refresh(first));
    // Lost response: the same token again within the grace window.
    const retry = await refresh(first);
    expect(retry.status).toBe(200);
    const b = await jsonOf<{ refreshToken: string }>(retry);
    // The successor the lost response carried is dead…
    expect((await refresh(a.refreshToken)).status).toBe(401);
    // …and the retried chain continues.
    const c = await jsonOf<{ refreshToken: string }>(await refresh(b.refreshToken));
    // Reusing `first` now (its successor was consumed) is theft: kill the session.
    expect((await refresh(first)).status).toBe(401);
    expect((await refresh(c.refreshToken)).status).toBe(401);
  });

  it("invites a second user (admin token required) and the invite works once", async () => {
    expect((await call("POST", "/admin/auth/invites", { email: "bob@example.com" })).status).toBe(401);
    const admin = { authorization: "Bearer test-admin-token" };
    const invite = await jsonOf<{ code: string; url: string }>(
      await call("POST", "/admin/auth/invites", { email: "bob@example.com" }, admin)
    );
    expect(invite.url).toContain("/auth/register?code=");
    expect((await register(await createFakePasskey(), "mallory@example.com", invite.code)).status).toBe(403);
    const bob = await createFakePasskey();
    const made = await register(bob, "bob@example.com", invite.code);
    expect(made.status, made.error).toBe(200);
    expect((await register(await createFakePasskey(), "bob@example.com", invite.code)).status).toBe(403);
    const users = await jsonOf<{ users: { user: { email: string }; passkeys: number }[] }>(
      await call("GET", "/admin/auth/users", undefined, admin)
    );
    expect(users.users.map((u) => u.user.email)).toEqual(["ada@example.com", "bob@example.com"]);
  });

  it("an invite for an existing email adds a second passkey to that account", async () => {
    const admin = { authorization: "Bearer test-admin-token" };
    const invite = await jsonOf<{ code: string }>(await call("POST", "/admin/auth/invites", { email: "ada@example.com" }, admin));
    const phone = await createFakePasskey();
    const made = await register(phone, "ada@example.com", invite.code);
    expect(made.status, made.error).toBe(200);
    const session = await jsonOf<Exchange>(await exchange(codeOf(made.redirect!)));
    expect(session.user.id).toBe(ownerSession.user.id);
    expect((await signIn(phone)).status).toBe(200);
  });

  it("revoking a user ends their sessions", async () => {
    const { body } = await signIn(owner);
    const session = await jsonOf<Exchange>(await exchange(codeOf(body.redirect!)));
    const admin = { authorization: "Bearer test-admin-token" };
    const revoked = await jsonOf<{ revoked: number }>(await call("POST", "/admin/auth/revoke", { userId: session.user.id }, admin));
    expect(revoked.revoked).toBeGreaterThan(0);
    expect((await refresh(session.refreshToken)).status).toBe(401);
  });

  it("refuses cross-origin calls to the browser endpoints", async () => {
    const res = await call("POST", "/auth/passkey/login/options", {}, { origin: "https://evil.example" });
    expect(res.status).toBe(403);
  });
});
