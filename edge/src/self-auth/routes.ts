/**
 * HTTP surface of the self-hosted identity provider (AUTH_MODE=self). All of
 * it runs before the bearer gate:
 *
 *   GET  /user_management/authorize      — the sign-in page engines open
 *                                          (WorkOS's path, so engines only
 *                                          need ZERON_WORKOS_API_BASE)
 *   GET  /auth/register[?code=]           — passkey setup / invite page
 *   POST /auth/passkey/login/options|verify
 *   POST /auth/passkey/register/options|verify
 *   POST /admin/auth/invites              — ADMIN_TOKEN: mint an invite
 *   GET  /admin/auth/users                — ADMIN_TOKEN: list accounts
 *   POST /admin/auth/revoke               — ADMIN_TOKEN: sign a user out everywhere
 *
 * `/auth/exchange`, `/auth/refresh` and `/auth/orgs` keep their WorkOS-era
 * wire shapes and dispatch here from auth-routes.ts (see `selfDirectory`).
 */
import type { Env } from "../env";
import { AUTH_DIRECTORY_NAME, type AuthDirectory, type RelyingParty, type Result } from "./directory";
import { signInPage } from "./page";
import { secretEquals } from "./tokens";

const json = (value: unknown, status = 200): Response =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store" }
  });

const html = (body: string, status = 200): Response =>
  new Response(body, {
    status,
    headers: {
      "content-type": "text/html; charset=utf-8",
      "cache-control": "no-store",
      // The page's own inline script and same-origin fetches only.
      "content-security-policy":
        "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; form-action 'none'; frame-ancestors 'none'; base-uri 'none'",
      "referrer-policy": "no-referrer"
    }
  });

export const selfAuthEnabled = (env: Pick<Env, "AUTH_MODE">): boolean => env.AUTH_MODE === "self";

export const selfDirectory = (env: Env): DurableObjectStub<AuthDirectory> | undefined => {
  if (!env.AUTH_DIRECTORY) return undefined;
  return env.AUTH_DIRECTORY.get(env.AUTH_DIRECTORY.idFromName(AUTH_DIRECTORY_NAME));
};

export const resultResponse = <T>(result: Result<T>): Response =>
  result.ok ? json(result.value) : json({ error: result.error }, result.status);

const relyingParty = (url: URL): RelyingParty => ({ rpID: url.hostname, origin: url.origin });

/**
 * Where a sign-in code may be sent: the engine's loopback callback, the
 * mobile app's custom scheme, or this edge's own headless paste-code page.
 * Anything else would hand a code to a third party.
 */
export const allowedRedirect = (raw: string | null | undefined, origin: string): URL | undefined => {
  if (!raw) return undefined;
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return undefined;
  }
  if (url.username || url.password) return undefined;
  if (url.protocol === "http:" && (url.hostname === "127.0.0.1" || url.hostname === "localhost") && url.pathname === "/callback") {
    return url;
  }
  if (url.protocol === "zeron:" && url.host === "callback" && (url.pathname === "" || url.pathname === "/")) return url;
  if (url.origin === origin && url.pathname === "/auth/cli/callback") return url;
  return undefined;
};

const withCode = (redirect: URL, code: string, state: string | null | undefined): string => {
  const out = new URL(redirect.toString());
  out.searchParams.set("code", code);
  if (state) out.searchParams.set("state", state);
  return out.toString();
};

const bodyJson = async <T>(request: Request): Promise<T | undefined> => {
  try {
    return (await request.json()) as T;
  } catch {
    return undefined;
  }
};

/** Browser-only JSON endpoints: same-origin callers only. */
const sameOrigin = (request: Request, url: URL): boolean => request.headers.get("origin") === url.origin;

const adminAuthorized = async (request: Request, env: Env): Promise<boolean> => {
  if (!env.ADMIN_TOKEN) return false;
  // Header only (never `?token=`: query strings end up in logs).
  const header = request.headers.get("authorization") ?? "";
  const token = header.toLowerCase().startsWith("bearer ") ? header.slice(7).trim() : "";
  return !!token && (await secretEquals(token, env.ADMIN_TOKEN));
};

/** Handle a self-auth route; undefined means "not one of ours". */
export const handleSelfAuthRoute = async (request: Request, env: Env, url: URL): Promise<Response | undefined> => {
  if (!selfAuthEnabled(env)) return undefined;
  const path = url.pathname;
  const isOurs =
    path === "/user_management/authorize" ||
    path === "/auth/register" ||
    path.startsWith("/auth/passkey/") ||
    path.startsWith("/admin/auth/");
  if (!isOurs) return undefined;
  const directory = selfDirectory(env);
  if (!directory || !env.AUTH_JWT_PRIVATE_KEY) return json({ error: "auth not configured" }, 501);

  if (path === "/user_management/authorize" && request.method === "GET") {
    const p = url.searchParams;
    const redirect = allowedRedirect(p.get("redirect_uri"), url.origin);
    if (p.get("response_type") !== "code" || !redirect || p.get("client_id") !== env.WORKOS_CLIENT_ID) {
      return html(signInPage({ registerFirst: false, error: "This sign-in link is not valid. Start sign-in again from Zeron." }), 400);
    }
    return html(
      signInPage({
        redirectUri: redirect.toString(),
        state: p.get("state") ?? undefined,
        registerFirst: await directory.needsSetup()
      })
    );
  }

  if (path === "/auth/register" && request.method === "GET") {
    return html(signInPage({ registerFirst: true, inviteCode: url.searchParams.get("code") ?? undefined }));
  }

  if (path.startsWith("/auth/passkey/")) {
    if (request.method !== "POST") return json({ error: "method not allowed" }, 405);
    if (!sameOrigin(request, url)) return json({ error: "forbidden" }, 403);
    const rp = relyingParty(url);
    const body = (await bodyJson<Record<string, unknown>>(request)) ?? {};
    const redirectRaw = typeof body.redirectUri === "string" ? body.redirectUri : undefined;
    const state = typeof body.state === "string" ? body.state : undefined;
    const redirect = allowedRedirect(redirectRaw, url.origin);
    if (redirectRaw && !redirect) return json({ error: "invalid redirect" }, 400);

    switch (path) {
      case "/auth/passkey/login/options":
        return resultResponse(await directory.loginOptions(rp));
      case "/auth/passkey/login/verify": {
        if (typeof body.challengeId !== "string" || typeof body.response !== "object" || body.response === null) {
          return json({ error: "bad request" }, 400);
        }
        const result = await directory.loginVerify(rp, body.challengeId, body.response as never);
        if (!result.ok) return resultResponse(result);
        return json({ redirect: redirect ? withCode(redirect, result.value.code, state) : null });
      }
      case "/auth/passkey/register/options":
        if (typeof body.email !== "string" || typeof body.code !== "string") return json({ error: "bad request" }, 400);
        return resultResponse(
          await directory.registerOptions(rp, {
            email: body.email,
            code: body.code,
            ...(typeof body.name === "string" ? { name: body.name } : {})
          })
        );
      case "/auth/passkey/register/verify": {
        if (typeof body.challengeId !== "string" || typeof body.response !== "object" || body.response === null) {
          return json({ error: "bad request" }, 400);
        }
        const result = await directory.registerVerify(rp, body.challengeId, body.response as never);
        if (!result.ok) return resultResponse(result);
        // Without an app waiting (an invite link), the code is simply dropped.
        return json({ redirect: redirect ? withCode(redirect, result.value.code, state) : null });
      }
    }
    return json({ error: "not found" }, 404);
  }

  // ── /admin/auth/* (operator) ──────────────────────────────────────────
  if (!(await adminAuthorized(request, env))) {
    return env.ADMIN_TOKEN ? json({ error: "unauthenticated" }, 401) : json({ error: "not_found" }, 404);
  }
  if (path === "/admin/auth/invites" && request.method === "POST") {
    const body = await bodyJson<{ email?: unknown; orgId?: unknown; ttlHours?: unknown }>(request);
    if (typeof body?.email !== "string") return json({ error: "missing email" }, 400);
    const result = await directory.createInvite({
      email: body.email,
      ...(typeof body.orgId === "string" ? { orgId: body.orgId } : {}),
      ...(typeof body.ttlHours === "number" && body.ttlHours > 0 ? { ttlMs: body.ttlHours * 3600_000 } : {})
    });
    if (!result.ok) return resultResponse(result);
    const link = new URL("/auth/register", url.origin);
    link.searchParams.set("code", result.value.code);
    return json({ ...result.value, url: link.toString() });
  }
  if (path === "/admin/auth/users" && request.method === "GET") {
    return json({ users: await directory.listUsers() });
  }
  if (path === "/admin/auth/revoke" && request.method === "POST") {
    const body = await bodyJson<{ userId?: unknown }>(request);
    if (typeof body?.userId !== "string") return json({ error: "missing userId" }, 400);
    return json(await directory.revokeUser(body.userId));
  }
  return json({ error: "not found" }, 404);
};
