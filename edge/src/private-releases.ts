import { authenticate } from "./auth";
import type { Env } from "./env";
import { json } from "./http";

/** Separate bucket: no object here is reachable through public /releases. */
export async function privateReleaseRoute(request: Request, env: Env, url: URL): Promise<Response | null> {
  if (!url.pathname.startsWith("/fork-updates/")) return null;
  const headers = { "cache-control": "private, no-store", vary: "Authorization, X-Zeron-Update-Token" };
  const error = (message: string, status: number) => {
    const response = json({ error: message }, status);
    for (const [key, value] of Object.entries(headers)) response.headers.set(key, value);
    return response;
  };
  if (!env.PRIVATE_RELEASES) return error("not_found", 404);
  // This capability only reads this bucket. It is never an account/session
  // token, so a local-only installation can update without joining sync.
  const supplied = request.headers.get("x-zeron-update-token");
  const expected = env.PRIVATE_RELEASES_TOKEN;
  const encoder = new TextEncoder();
  const suppliedBytes = encoder.encode(supplied ?? "");
  const expectedBytes = encoder.encode(expected ?? "");
  const allowed = expected && suppliedBytes.length === expectedBytes.length &&
    crypto.subtle.timingSafeEqual(suppliedBytes, expectedBytes);
  if (!allowed) {
    // Retain the owner-session path for already installed releases.
    const auth = await authenticate(env, request);
    if (!auth) return error("unauthenticated", 401);
    if (auth.kind !== "user" || !env.PRIVATE_RELEASES_USER_ID || auth.userId !== env.PRIVATE_RELEASES_USER_ID ||
        !request.headers.get("authorization")?.toLowerCase().startsWith("bearer ")) {
      return error("forbidden", 403);
    }
  }
  if (request.method !== "GET" && request.method !== "HEAD") return error("method_not_allowed", 405);
  let key: string;
  try { key = decodeURIComponent(url.pathname.slice("/fork-updates/".length)); }
  catch { return error("bad_request", 400); }
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,180}$/.test(key) || key.includes("..")) return error("bad_request", 400);
  const object = await env.PRIVATE_RELEASES.get(key);
  if (!object) return error("not_found", 404);
  return new Response(request.method === "HEAD" ? null : object.body, { headers: {
    ...headers, "content-type": key.endsWith(".json") ? "application/json" : "application/octet-stream",
    "content-length": String(object.size), etag: object.httpEtag
  } });
}
