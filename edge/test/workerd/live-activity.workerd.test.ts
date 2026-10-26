import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { AUTH_USER_HEADER } from "../../src/env";
import type { Op } from "../../src/registry-core";

const H = { [AUTH_USER_HEADER]: "user" };
const TOKEN = "ab".repeat(32);
let tick = 1;
const hlc = () => `${String(tick++).padStart(13, "0")}-000000-mac`;
const session = (status: string, turn: string | null, updatedAt = Date.now()): Op => ({
  kind: "sessions",
  id: "c1",
  op: "upsert",
  hlc: hlc(),
  set: { chatId: "c1", deviceId: "mac", status, lastCompletedTurn: turn, updatedAt, startedAt: updatedAt }
});

function room() {
  return env.REGISTRY_ROOMS.get(env.REGISTRY_ROOMS.idFromName(crypto.randomUUID()));
}
async function push(stub: DurableObjectStub, ops: Op[]) {
  const r = await stub.fetch("https://registry/push?device=mac", { method: "POST", headers: H, body: JSON.stringify({ batch: crypto.randomUUID(), ops }) });
  expect(r.status).toBe(200);
}
async function liveActivity(stub: DurableObjectStub, device: string, body: Record<string, unknown>) {
  return stub.fetch(`https://registry/live-activity?device=${device}`, { method: "POST", headers: H, body: JSON.stringify(body) });
}
interface Stats {
  pushLog: Array<{ chatId: string; category: string; device: string; result: string }>;
  liveActivity: {
    targets: Array<{ device: string; canStart: boolean }>;
    activities: Array<{ device: string; activityId: string | null; confirmed: boolean; state: { entries: Array<{ title: string; host: string; status: string }> } }>;
    watches: Array<{ device: string; chatId: string; status: string }>;
    log: Array<{ device: string; event: string; category?: string; result: string }>;
  };
}
async function stats(stub: DurableObjectStub) {
  return (await stub.fetch("https://registry/stats", { headers: H })).json<Stats>();
}

describe("session Live Activity on a real RegistryRoom", () => {
  it("starts on a phone's send, updates with the chime, and ends with the last session", async () => {
    const stub = room();
    await stub.fetch("https://registry/push-target?device=ios-1", { method: "POST", headers: H, body: JSON.stringify({ token: TOKEN, environment: "production" }) });
    expect((await liveActivity(stub, "ios-1", { startToken: TOKEN, environment: "production" })).status).toBe(200);
    await push(stub, [
      { kind: "devices", id: "mac", op: "upsert", hlc: hlc(), set: { name: "MacBook Pro", platform: "macos" } },
      { kind: "chats", id: "c1", op: "upsert", hlc: hlc(), set: { title: "Fix login", deviceId: "mac", config: { harness: "codex" } } },
      session("idle", "t0")
    ]);

    // The phone sends: the activity starts (no APNs key here: decided, logged).
    expect((await liveActivity(stub, "ios-1", { watch: "c1" })).status).toBe(200);
    let s = await stats(stub);
    expect(s.liveActivity.log.map((e) => [e.event, e.result])).toEqual([["start", "not configured"]]);
    expect(s.liveActivity.activities).toMatchObject([{ device: "ios-1", activityId: null, confirmed: false }]);
    expect(s.liveActivity.activities[0].state.entries[0]).toMatchObject({ title: "Fix login", host: "MacBook Pro", status: "starting" });

    // The started activity reports its update token; the state is unchanged.
    expect((await liveActivity(stub, "ios-1", { activityId: "act-1", token: TOKEN })).status).toBe(200);
    await push(stub, [session("working", "t0")]);
    await push(stub, [session("working", "t0")]); // heartbeat
    await push(stub, [session("awaitingInput", "t0")]);
    await push(stub, [session("working", "t0")]);
    await push(stub, [session("idle", "t1")]);
    s = await stats(stub);
    expect(s.liveActivity.log.map((e) => [e.event, e.category ?? null])).toEqual([
      ["start", null],
      ["update", null],
      ["update", "input"],
      ["update", null],
      ["end", null]
    ]);
    // The question sounded on the activity; the final completion is the
    // plain notification's.
    expect(s.pushLog.map((e) => [e.category, e.result])).toEqual([
      ["input", "live activity"],
      ["done", "not configured"]
    ]);
    expect(s.liveActivity.activities).toEqual([]);
    expect(s.liveActivity.watches).toEqual([]);
  });

  it("ignores sends from a phone without Live Activities, and forgets one turned off", async () => {
    const stub = room();
    expect((await liveActivity(stub, "ios-2", { watch: "c1" })).status).toBe(200);
    expect((await stats(stub)).liveActivity.watches).toEqual([]);

    await liveActivity(stub, "ios-2", { startToken: TOKEN, environment: "sandbox" });
    await liveActivity(stub, "ios-2", { watch: "c1" });
    let s = await stats(stub);
    expect(s.liveActivity.watches).toMatchObject([{ device: "ios-2", chatId: "c1", status: "starting" }]);
    expect(s.liveActivity.targets).toEqual([{ device: "ios-2", environment: "sandbox", canStart: true }]);

    await stub.fetch("https://registry/live-activity?device=ios-2", { method: "DELETE", headers: H });
    s = await stats(stub);
    expect(s.liveActivity.targets).toEqual([]);
    expect(s.liveActivity.watches).toEqual([]);
    expect(s.liveActivity.activities).toEqual([]);
  });

  it("rejects malformed registrations", async () => {
    const stub = room();
    expect((await liveActivity(stub, "ios-3", { startToken: "nope", environment: "production" })).status).toBe(400);
    expect((await liveActivity(stub, "ios-3", { activityId: "a", token: "nope" })).status).toBe(400);
    expect((await liveActivity(stub, "ios-3", {})).status).toBe(400);
    expect((await stub.fetch("https://registry/live-activity", { method: "POST", headers: H, body: "{}" })).status).toBe(400);
  });
});
