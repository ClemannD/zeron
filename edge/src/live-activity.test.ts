import { describe, expect, it } from "vitest";
import {
  activityPayload,
  deriveWatch,
  DISMISS_AFTER_MS,
  MAX_AGE_MS,
  newWatch,
  plan,
  START_GRACE_MS,
  START_TIMEOUT_MS,
  type ActivityRecord,
  type Watch,
  type WatchContext
} from "./live-activity";
import { parsePrefs } from "./push-notify";
import type { Row } from "./registry-core";

const NOW = 1_800_000_000_000;

const session = (status: string, turn: string | null, extra: Row["fields"] = {}, updatedAt = NOW): Row => ({
  kind: "sessions",
  id: "c1",
  seq: 1,
  deleted: false,
  fields: { chatId: "c1", status, updatedAt, ...(turn === null ? {} : { lastCompletedTurn: turn }), ...extra },
  clocks: {}
});
const chat = (id: string, title: string): Row => ({
  kind: "chats",
  id,
  seq: 1,
  deleted: false,
  fields: { title, deviceId: "mac", config: { harness: "codex" } },
  clocks: {}
});

const sent = (turn: string | null = "t0", at = NOW): Watch => newWatch("c1", session("idle", turn), at);

describe("deriveWatch", () => {
  it("is starting until the host picks the send up, then gone", () => {
    const w = sent();
    expect(deriveWatch(w, session("idle", "t0"), NOW + 1000)?.status).toBe("starting");
    expect(deriveWatch(w, session("idle", "t0", {}, NOW - 1), NOW + START_GRACE_MS + 1)).toBeNull();
  });

  it("follows the run to done, keyed by a new completed turn", () => {
    let w = deriveWatch(sent(), session("working", "t0"), NOW)!;
    expect(w).toMatchObject({ status: "working", seen: true });
    w = deriveWatch(w, session("awaitingInput", "t0"), NOW)!;
    expect(w.status).toBe("awaiting");
    w = deriveWatch(w, session("idle", "t1"), NOW + 5000)!;
    expect(w).toMatchObject({ status: "done", final: "done", finishedAt: NOW + 5000 });
    // Settled: stays done (and keeps its time) on later heartbeats.
    expect(deriveWatch(w, session("idle", "t1"), NOW + 9000)).toMatchObject({ status: "done", finishedAt: NOW + 5000 });
  });

  it("an interrupt is stopped; a failure is failed", () => {
    const running = deriveWatch(sent(), session("working", "t0"), NOW)!;
    expect(deriveWatch(running, session("idle", "t0"), NOW)?.status).toBe("stopped");
    expect(deriveWatch(running, session("errored", "t0"), NOW)?.status).toBe("failed");
  });

  it("an old failure is not this send's", () => {
    const w = newWatch("c1", session("errored", "t0", {}, NOW - 60_000), NOW);
    expect(deriveWatch(w, session("errored", "t0", {}, NOW - 60_000), NOW + 1000)?.status).toBe("starting");
  });

  it("a dead host's stale Working row is stopped", () => {
    const running = deriveWatch(sent(), session("working", "t0"), NOW)!;
    expect(deriveWatch(running, session("working", "t0", {}, NOW - 60_000), NOW)?.status).toBe("stopped");
  });

  it("a queued message after a completed turn runs again", () => {
    const done = deriveWatch(deriveWatch(sent(), session("working", "t0"), NOW)!, session("idle", "t1"), NOW)!;
    expect(deriveWatch(done, session("working", "t1"), NOW)?.status).toBe("working");
  });
});

const ctx = (row: Row, title = "Fix login"): WatchContext => ({ chat: chat("c1", title), session: row, host: "MacBook Pro" });
const prefs = parsePrefs(undefined);
const confirmed = (state: ActivityRecord["state"], startedAt = NOW - 1000): ActivityRecord => ({
  activityId: "a1",
  token: "ab".repeat(32),
  startedAt,
  state
});

describe("plan", () => {
  it("starts on the first send with a push-to-start token", () => {
    const p = plan({ watches: [{ watch: sent(), ctx: ctx(session("idle", "t0")) }], activity: null, startToken: "cd", prefs, now: NOW });
    expect(p.sends).toHaveLength(1);
    expect(p.sends[0]).toMatchObject({ event: "start", title: "Fix login", host: "MacBook Pro" });
    expect(p.activity).toMatchObject({ activityId: null, token: null, startedAt: NOW });
    expect(p.sends[0].state.entries[0]).toEqual({
      id: "c1",
      title: "Fix login",
      host: "MacBook Pro",
      harness: "codex",
      status: "starting",
      since: NOW / 1000
    });
  });

  it("waits for the phone's update token before updating", () => {
    const pending: ActivityRecord = { activityId: null, token: null, startedAt: NOW, state: { entries: [], running: 1, awaiting: 0, done: 0, failed: 0, more: 0 } };
    const p = plan({ watches: [{ watch: sent(), ctx: ctx(session("working", "t0")) }], activity: pending, startToken: "cd", prefs, now: NOW });
    expect(p.sends).toEqual([]);
    expect(p.activity).toBeUndefined();
    const late = plan({ watches: [{ watch: sent(), ctx: ctx(session("working", "t0")) }], activity: pending, startToken: "cd", prefs, now: NOW + START_TIMEOUT_MS + 1 });
    expect(late).toMatchObject({ clear: true, activity: null, sends: [] });
  });

  it("updates only on change, with the desktop's chime on a question", () => {
    const working = plan({ watches: [{ watch: sent(), ctx: ctx(session("working", "t0", { startedAt: NOW - 3000 })) }], activity: null, startToken: "cd", prefs, now: NOW });
    const activity = confirmed(working.sends[0].state);
    const watch = working.watches[0];
    // Heartbeat: same state, nothing sent.
    expect(plan({ watches: [{ watch, ctx: ctx(session("working", "t0", { startedAt: NOW - 3000 })) }], activity, startToken: "cd", prefs, now: NOW + 1000 }).sends).toEqual([]);
    const asking = plan({ watches: [{ watch, ctx: ctx(session("awaitingInput", "t0", { startedAt: NOW - 3000 })) }], activity, startToken: "cd", prefs, now: NOW + 2000 });
    expect(asking.sends).toHaveLength(1);
    expect(asking.sends[0]).toMatchObject({ event: "update", alert: { chatId: "c1", category: "input", title: "Fix login" } });
    expect(asking.sends[0].state.awaiting).toBe(1);
    const payload = activityPayload(asking.sends[0], NOW + 2000);
    expect(payload.aps).toMatchObject({
      event: "update",
      timestamp: (NOW + 2000) / 1000,
      alert: { title: "Fix login", body: "Waiting on your input", sound: "request.wav" }
    });
  });

  it("honors the phone's notification choices for alerts", () => {
    const running = deriveWatch(sent(), session("working", "t0"), NOW)!;
    const activity = confirmed({ entries: [], running: 1, awaiting: 0, done: 0, failed: 0, more: 0 });
    const p = plan({ watches: [{ watch: running, ctx: ctx(session("awaitingInput", "t0")) }], activity, startToken: "cd", prefs: { ...prefs, input: false }, now: NOW });
    expect(p.sends[0]).toEqual({ event: "update", state: expect.anything() });
  });

  it("a finished session alerts while another still runs, and the last one ends it quietly", () => {
    const a = deriveWatch(sent(), session("working", "t0"), NOW)!;
    const b = { ...deriveWatch(sent(), session("working", "t0"), NOW)!, chatId: "c2" };
    const activity = confirmed({ entries: [], running: 2, awaiting: 0, done: 0, failed: 0, more: 0 });
    const mid = plan({
      watches: [
        { watch: a, ctx: ctx(session("idle", "t1"), "First") },
        { watch: b, ctx: { ...ctx(session("working", "t0")), chat: chat("c2", "Second") } }
      ],
      activity,
      startToken: "cd",
      prefs,
      now: NOW
    });
    expect(mid.sends[0]).toMatchObject({ event: "update", alert: { chatId: "c1", category: "done" } });
    expect(mid.sends[0].state).toMatchObject({ running: 1, done: 1 });
    // Running first, then the finished one.
    expect(mid.sends[0].state.entries.map((e) => e.status)).toEqual(["working", "done"]);

    const [a2, b2] = mid.watches;
    const last = plan({
      watches: [
        { watch: a2, ctx: ctx(session("idle", "t1"), "First") },
        { watch: b2, ctx: { ...ctx(session("idle", "t1")), chat: chat("c2", "Second") } }
      ],
      activity: { ...activity, state: mid.sends[0].state },
      startToken: "cd",
      prefs,
      now: NOW + 1000
    });
    expect(last).toMatchObject({ clear: true, activity: null });
    expect(last.sends).toEqual([{ event: "end", state: expect.objectContaining({ done: 2, running: 0 }), dismissAt: NOW + 1000 + DISMISS_AFTER_MS }]);
    expect(activityPayload(last.sends[0], NOW + 1000).aps).not.toHaveProperty("alert");
  });

  it("hands over to a fresh activity before ActivityKit's 8 hours", () => {
    const watch = deriveWatch(sent(), session("working", "t0"), NOW)!;
    const activity = confirmed({ entries: [], running: 1, awaiting: 0, done: 0, failed: 0, more: 0 }, NOW - MAX_AGE_MS - 1);
    const p = plan({ watches: [{ watch, ctx: ctx(session("working", "t0")) }], activity, startToken: "cd", prefs, now: NOW });
    expect(p.sends.map((s) => s.event)).toEqual(["end", "start"]);
    expect(p.activity).toMatchObject({ token: null, startedAt: NOW });
  });

  it("without a push-to-start token, keeps watching but sends nothing", () => {
    const p = plan({ watches: [{ watch: sent(), ctx: ctx(session("working", "t0")) }], activity: null, startToken: null, prefs, now: NOW });
    expect(p).toMatchObject({ clear: false, sends: [] });
  });

  it("caps the entries and counts the rest", () => {
    const watches = ["a", "b", "c", "d", "e", "f"].map((id) => ({
      watch: { ...deriveWatch(sent(), session("working", "t0"), NOW)!, chatId: id },
      ctx: { chat: chat(id, id), session: session("working", "t0") }
    }));
    const p = plan({ watches, activity: null, startToken: "cd", prefs, now: NOW });
    expect(p.sends[0].state).toMatchObject({ running: 6, more: 2 });
    expect(p.sends[0].state.entries).toHaveLength(4);
  });

  it("keeps the content state well under ActivityKit's 4 KB", () => {
    const long = "x".repeat(500);
    const watches = ["a", "b", "c", "d", "e"].map((id) => ({
      watch: { ...deriveWatch(sent(), session("working", "t0"), NOW)!, chatId: id.repeat(36) },
      ctx: { chat: chat(id, long), session: session("working", "t0"), host: long }
    }));
    const p = plan({ watches, activity: null, startToken: "cd", prefs, now: NOW });
    expect(JSON.stringify(activityPayload(p.sends[0], NOW)).length).toBeLessThan(3500);
  });
});
