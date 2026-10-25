import type { Verified } from "./auth";
import type { Env } from "./env";
import { json } from "./http";

/** Separate bucket: no object here is reachable through public /releases. */
export async function privateReleaseRoute(request: Request, env: Env, auth: Verified, url: URL): Promise<Response | null> {
  if (!url.pathname.startsWith("/fork-updates/")) return null;
  const headers = { "cache-control": "private, no-store", vary: "Authorization" };
  const error = (message: string, status: number) => {
    const response = json({ error: message }, status);
    for (const [key, value] of Object.entries(headers)) response.headers.set(key, value);
    return response;
  };
  if (!env.PRIVATE_RELEASES || !env.PRIVATE_RELEASES_USER_ID) return error("not_found", 404);
  if (auth.kind !== "user" || auth.userId !== env.PRIVATE_RELEASES_USER_ID ||
      !request.headers.get("authorization")?.toLowerCase().startsWith("bearer ")) {
    return error("forbidden", 403);
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
