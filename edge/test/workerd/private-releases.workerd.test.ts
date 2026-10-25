import { env, SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { authenticate } from "../../src/auth";
import { privateReleaseRoute } from "../../src/private-releases";
import { mintAccessToken } from "../../src/self-auth/tokens";

describe("private fork releases", () => {
  it("requires the allowed user's session and never exposes private objects through the public release bucket", async () => {
    await env.PRIVATE_RELEASES!.put("manifest.json", '{"version":"0.2.124.1"}');
    await env.PRIVATE_RELEASES!.put("private-app.tar.gz", "private-payload");
    expect((await SELF.fetch("https://test/fork-updates/manifest.json")).status).toBe(401);
    const headers = { authorization: "Bearer release-owner@org1" };
    const allowed = await SELF.fetch("https://test/fork-updates/manifest.json", { headers });
    expect(allowed.status).toBe(200);
    expect(allowed.headers.get("cache-control")).toBe("private, no-store");
    expect(allowed.headers.get("vary")).toBe("Authorization");
    expect(await allowed.json()).toEqual({ version: "0.2.124.1" });
    expect((await SELF.fetch("https://test/fork-updates/manifest.json", { headers: { authorization: "Bearer other@org1" } })).status).toBe(403);
    expect((await SELF.fetch("https://test/fork-updates/manifest.json?token=release-owner@org1")).status).toBe(403);
    expect((await SELF.fetch("https://test/fork-updates/manifest.json", { headers, method: "PUT", body: "overwrite" })).status).toBe(405);
    expect((await SELF.fetch("https://test/fork-updates/bad%2Fkey", { headers })).status).toBe(400);
    expect((await SELF.fetch("https://test/fork-updates/bad%ZZ", { headers })).status).toBe(400);
    expect((await SELF.fetch("https://test/releases/private-app.tar.gz")).status).toBe(404);
    const head = await SELF.fetch("https://test/fork-updates/private-app.tar.gz", { headers, method: "HEAD" });
    expect(head.status).toBe(200);
    expect(await head.text()).toBe("");
  });

  it("accepts our real self-auth JWTs and refuses runner tokens", async () => {
    const token = await mintAccessToken(env.AUTH_JWT_PRIVATE_KEY!, { userId: "release-owner", sessionId: "release-session", orgId: "org1" });
    await env.PRIVATE_RELEASES!.put("signed-test.json", "{}");
    const request = new Request("https://test/fork-updates/signed-test.json", {
      headers: { authorization: `Bearer ${token}` }
    });
    const selfEnv = { ...env, AUTH_MODE: "self" };
    const auth = await authenticate(selfEnv, request);
    expect(auth?.userId).toBe("release-owner");
    const response = await privateReleaseRoute(request, selfEnv, auth!, new URL(request.url));
    expect(response!.status).toBe(200);
    await response!.arrayBuffer();
    const runner = await SELF.fetch("https://test/fork-updates/signed-test.json", {
      headers: { authorization: "Bearer runner:release-owner@org1:runner1" }
    });
    expect(runner.status).toBe(403);
  });
});
