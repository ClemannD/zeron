/**
 * The phone's session Live Activity: one combined activity per phone for the
 * sessions that phone sent a message to (a new session, a follow-up, an
 * answer). The phone reports each send (`POST /live-activity {watch}`) and
 * hands over its ActivityKit tokens; this module derives each watched
 * session's state from its `sessions` row and starts, updates and ends the
 * activity over APNs.
 *
 * Planning is pure (`plan`, unit tested in live-activity.test.ts);
 * `LiveActivities` keeps the private tables and sends, driven by RegistryRoom
 * after every batch and by its alarm while an activity is up (a dead host
 * sends no batch, so only the sweep notices its session went stale).
 *
 * Alerts (a session needs input, finished or failed while others still run)
 * ride the activity update with the desktop's chime; RegistryRoom skips the
 * plain notification for that phone and chat so nothing sounds twice. The
 * last session finishing ends the activity silently and leaves the alert to
 * the plain notification.
 */
import { isDeadToken, sendApns, type ApnsConfig, type ApnsEnvironment } from "./apns";
import { bodyFor, indicatorOf, SOUNDS, type Category, type PushPrefs } from "./push-notify";
import type { FieldValue, Row } from "./registry-core";

export type ActivityStatus = "starting" | "working" | "awaiting" | "done" | "failed" | "stopped";

/** A session the phone sent a message to, from that send on. */
export interface Watch {
  chatId: string;
  /** When the phone sent (ms). */
  since: number;
  /** `lastCompletedTurn` at the send: a different one is a finished turn. */
  baseTurn: string | null;
  /** Went Working or AwaitingInput since the send. */
  seen: boolean;
  final: "done" | "failed" | null;
  finishedAt: number | null;
  /** Last derived status: alerts fire on changes. */
  status: ActivityStatus | null;
}

/** One row of the activity (Swift `SessionsActivityEntry`). */
export interface ActivityEntry {
  id: string;
  title: string;
  host: string;
  harness: string;
  status: ActivityStatus;
  /** Epoch seconds: run start while running, finish time once finished. */
  since: number;
}

/** The activity's content state (Swift `SessionsActivityAttributes.ContentState`). */
export interface ActivityState {
  entries: ActivityEntry[];
  running: number;
  awaiting: number;
  done: number;
  failed: number;
  /** Sessions past the shown entries. */
  more: number;
}

/** The activity this room believes the phone shows. */
export interface ActivityRecord {
  /** Unknown until the phone reports the started activity's update token. */
  activityId: string | null;
  token: string | null;
  startedAt: number;
  state: ActivityState;
}

/** A session's surroundings: its rows and its host's name. */
export interface WatchContext {
  chat?: Row;
  session?: Row;
  host?: string;
}

/** Must match the Swift attributes type's name. */
export const ATTRIBUTES_TYPE = "SessionsActivityAttributes";
/** A send whose host never picks it up drops out after this. */
export const START_GRACE_MS = 3 * 60_000;
/** ActivityKit ends an activity after 8 hours; hand over before that. */
export const MAX_AGE_MS = 7.5 * 60 * 60_000;
/** A start the phone never confirmed (no update token): forget it. */
export const START_TIMEOUT_MS = 10 * 60_000;
/** How long the final state stays on the Lock Screen. */
export const DISMISS_AFTER_MS = 15 * 60_000;
/** Sweep cadence while an activity is up (stale hosts, start grace). */
export const SWEEP_MS = 30_000;
export const MAX_ENTRIES = 4;
const TITLE_MAX = 60;

const num = (v: FieldValue | undefined): number | undefined => (typeof v === "number" ? v : undefined);
const str = (v: FieldValue | undefined): string | undefined => (typeof v === "string" ? v : undefined);

export const isActive = (status: ActivityStatus): boolean =>
  status === "starting" || status === "working" || status === "awaiting";

/** The watch a send starts (a follow-up restarts it; `status` carries over). */
export const newWatch = (chatId: string, session: Row | undefined, now: number, previous?: Watch): Watch => ({
  chatId,
  since: now,
  baseTurn: str(session?.fields.lastCompletedTurn) ?? null,
  seen: false,
  final: null,
  finishedAt: null,
  status: previous?.status ?? null
});

/** The watch after its session row; null once it no longer belongs (the
 * host never picked the send up). Running beats finished: a queued message
 * after a completed turn shows as working again. */
export const deriveWatch = (watch: Watch, session: Row | undefined, now: number): Watch | null => {
  const fields = session && !session.deleted ? session.fields : {};
  const indicator = indicatorOf(fields, now);
  if (indicator === "working" || indicator === "awaiting") return { ...watch, seen: true, status: indicator };
  const turn = str(fields.lastCompletedTurn) ?? null;
  if (indicator === "errored" && (num(fields.updatedAt) ?? 0) >= watch.since) {
    const finishedAt = watch.final === "failed" ? watch.finishedAt : now;
    return { ...watch, seen: true, final: "failed", finishedAt, status: "failed" };
  }
  if (turn !== null && turn !== watch.baseTurn) {
    return { ...watch, baseTurn: turn, seen: true, final: "done", finishedAt: now, status: "done" };
  }
  if (watch.final) return { ...watch, status: watch.final };
  if (watch.seen) return { ...watch, finishedAt: watch.finishedAt ?? now, status: "stopped" };
  if (now - watch.since < START_GRACE_MS) return { ...watch, status: "starting" };
  return null;
};

/** The alert a status change calls for. A first derivation is a baseline. */
export const alertFor = (previous: ActivityStatus | null, next: ActivityStatus): Category | null => {
  if (previous === null || previous === next) return null;
  if (next === "awaiting") return "input";
  if (next === "failed") return "failed";
  if (next === "done") return "done";
  return null;
};

const HOST_MAX = 32;

const clip = (text: string, max = TITLE_MAX): string => (text.length > max ? `${text.slice(0, max - 1).trimEnd()}…` : text);

const harnessOf = (chat: Row["fields"]): string => {
  const config = chat.config;
  if (config && typeof config === "object" && !Array.isArray(config)) return str(config.harness) ?? "";
  return "";
};

export const entryFor = (watch: Watch, ctx: WatchContext, now: number): ActivityEntry => {
  const chat = ctx.chat && !ctx.chat.deleted ? ctx.chat.fields : {};
  const status = watch.status ?? "starting";
  const running = status === "working" || status === "awaiting";
  const at = running
    ? (num(ctx.session?.fields.startedAt) ?? watch.since)
    : status === "starting"
      ? watch.since
      : (watch.finishedAt ?? now);
  return {
    id: watch.chatId,
    title: clip(str(chat.title)?.trim() || "New session"),
    host: clip(ctx.host ?? "", HOST_MAX),
    harness: harnessOf(chat),
    status,
    since: Math.floor(at / 1000)
  };
};

/** Waiting on you first, then the longest running, then the latest finished. */
const rank = (e: ActivityEntry): number => (e.status === "awaiting" ? 0 : isActive(e.status) ? 1 : 2);

export const stateOf = (entries: ActivityEntry[]): ActivityState => {
  const sorted = [...entries].sort(
    (a, b) => rank(a) - rank(b) || (rank(a) === 2 ? b.since - a.since : a.since - b.since) || a.id.localeCompare(b.id)
  );
  const count = (...s: ActivityStatus[]) => entries.filter((e) => s.includes(e.status)).length;
  return {
    entries: sorted.slice(0, MAX_ENTRIES),
    running: count("starting", "working"),
    awaiting: count("awaiting"),
    done: count("done"),
    failed: count("failed"),
    more: Math.max(0, sorted.length - MAX_ENTRIES)
  };
};

export interface ActivityAlert {
  chatId: string;
  title: string;
  category: Category;
}

export type Send =
  | { event: "start"; state: ActivityState; title: string; host: string }
  | { event: "update"; state: ActivityState; alert?: ActivityAlert }
  | { event: "end"; state: ActivityState; dismissAt: number };

export interface Plan {
  /** Derived watches to keep. */
  watches: Watch[];
  /** Watches to drop (never picked up). */
  drop: string[];
  /** The cycle is over: drop every watch. */
  clear: boolean;
  /** New activity record; null deletes it, undefined leaves it. */
  activity?: ActivityRecord | null;
  sends: Send[];
}

const ALERT_ORDER: Category[] = ["input", "failed", "done"];

/** What one phone's activity should do now. */
export const plan = (input: {
  watches: Array<{ watch: Watch; ctx: WatchContext }>;
  activity: ActivityRecord | null;
  startToken: string | null;
  prefs: PushPrefs;
  now: number;
}): Plan => {
  const { activity, startToken, prefs, now } = input;
  const watches: Watch[] = [];
  const drop: string[] = [];
  const entries: ActivityEntry[] = [];
  let alert: ActivityAlert | undefined;
  for (const { watch, ctx } of input.watches) {
    const next = deriveWatch(watch, ctx.session, now);
    if (next === null) {
      drop.push(watch.chatId);
      continue;
    }
    watches.push(next);
    const entry = entryFor(next, ctx, now);
    entries.push(entry);
    const category = alertFor(watch.status, next.status ?? "starting");
    if (category && prefs[category] && (!alert || ALERT_ORDER.indexOf(category) < ALERT_ORDER.indexOf(alert.category))) {
      alert = { chatId: next.chatId, title: entry.title, category };
    }
  }
  const state = stateOf(entries);
  const active = entries.some((e) => isActive(e.status));
  const fresh = (): ActivityRecord => ({ activityId: null, token: null, startedAt: now, state });
  const start = (): Send => ({ event: "start", state, title: state.entries[0]?.title ?? "Zeron", host: state.entries[0]?.host ?? "" });

  if (activity === null) {
    if (!active) return { watches, drop, clear: true, sends: [] };
    if (startToken === null) return { watches, drop, clear: false, sends: [] };
    return { watches, drop, clear: false, activity: fresh(), sends: [start()] };
  }
  if (activity.token === null) {
    // Started; the phone has not reported the activity yet.
    if (now - activity.startedAt > START_TIMEOUT_MS) return { watches, drop, clear: true, activity: null, sends: [] };
    return { watches, drop, clear: false, sends: [] };
  }
  if (!active) {
    return { watches, drop, clear: true, activity: null, sends: [{ event: "end", state, dismissAt: now + DISMISS_AFTER_MS }] };
  }
  if (now - activity.startedAt > MAX_AGE_MS && startToken !== null) {
    return { watches, drop, clear: false, activity: fresh(), sends: [{ event: "end", state, dismissAt: now }, start()] };
  }
  if (JSON.stringify(state) === JSON.stringify(activity.state)) return { watches, drop, clear: false, sends: [] };
  return {
    watches,
    drop,
    clear: false,
    activity: { ...activity, state },
    sends: [alert ? { event: "update", state, alert } : { event: "update", state }]
  };
};

/** The ActivityKit push body for one send. */
export const activityPayload = (send: Send, now: number) => {
  const aps: Record<string, unknown> = {
    timestamp: Math.floor(now / 1000),
    event: send.event,
    "content-state": send.state
  };
  if (send.event === "start") {
    aps["attributes-type"] = ATTRIBUTES_TYPE;
    aps.attributes = {};
    // Quiet: it lights the Lock Screen, no sound (the phone just sent).
    aps.alert = { title: send.title, body: send.host ? `Sent to ${send.host}` : "Sent" };
  } else if (send.event === "update" && send.alert) {
    aps.alert = { title: send.alert.title, body: bodyFor(send.alert.category), sound: SOUNDS[send.alert.category] };
  } else if (send.event === "end") {
    aps["dismissal-date"] = Math.floor(send.dismissAt / 1000);
  }
  return { aps };
};

// ── storage + delivery ────────────────────────────────────────────────────

/** What LiveActivities needs from its room. */
export interface LiveActivityHost {
  sql: SqlStorage;
  loadRow(kind: string, id: string): Row | undefined;
  prefs(device: string): PushPrefs;
  apns(): ApnsConfig | undefined;
  waitUntil(promise: Promise<unknown>): void;
  /** Ask for `sweep` at (or before) `at`. */
  scheduleSweep(at: number): void;
}

interface LogEntry {
  at: number;
  device: string;
  event: Send["event"];
  chatId?: string;
  category?: Category;
  result: string;
}

const LOG_MAX = 30;
const HEX_RE = /^[0-9a-fA-F]{32,512}$/;
const ID_RE = /^[A-Za-z0-9_.:-]{1,128}$/;

export class LiveActivities {
  private readonly log: LogEntry[] = [];

  constructor(private readonly host: LiveActivityHost) {
    const sql = host.sql;
    // Private to this room, like push_targets: never rows.
    sql.exec(
      "CREATE TABLE IF NOT EXISTS la_targets (device TEXT PRIMARY KEY, start_token TEXT, environment TEXT NOT NULL, updated_at INTEGER NOT NULL)"
    );
    sql.exec(
      "CREATE TABLE IF NOT EXISTS la_watches (device TEXT NOT NULL, chat_id TEXT NOT NULL, watch TEXT NOT NULL, PRIMARY KEY (device, chat_id))"
    );
    sql.exec(
      "CREATE TABLE IF NOT EXISTS la_activities (device TEXT PRIMARY KEY, activity TEXT NOT NULL)"
    );
  }

  /** `POST /live-activity`: a push-to-start token, a started activity's
   * update token, a send to watch, or an activity the phone saw end. */
  register(device: string, body: Record<string, unknown>, now = Date.now()): { status: number; error?: string } {
    const sql = this.host.sql;
    if (typeof body.startToken === "string") {
      const environment = body.environment === "sandbox" ? "sandbox" : body.environment === "production" ? "production" : undefined;
      if (!HEX_RE.test(body.startToken) || environment === undefined) return { status: 400, error: "startToken and environment required" };
      sql.exec(
        "INSERT INTO la_targets (device, start_token, environment, updated_at) VALUES (?, ?, ?, ?) ON CONFLICT(device) DO UPDATE SET start_token = excluded.start_token, environment = excluded.environment, updated_at = excluded.updated_at",
        device,
        body.startToken,
        environment,
        now
      );
      this.evaluate(device, now);
      return { status: 200 };
    }
    if (typeof body.activityId === "string" && typeof body.token === "string") {
      if (!ID_RE.test(body.activityId) || !HEX_RE.test(body.token)) return { status: 400, error: "activityId and token required" };
      // The phone's activity is the truth: adopt it (a start this room sent,
      // or one that outlived this room's record and should end).
      const current = this.activity(device);
      const ours = current !== null && (current.activityId === null || current.activityId === body.activityId);
      this.putActivity(device, {
        activityId: body.activityId,
        token: body.token,
        startedAt: ours ? current.startedAt : now,
        state: current?.state ?? stateOf([])
      });
      this.evaluate(device, now);
      return { status: 200 };
    }
    if (typeof body.watch === "string") {
      if (!ID_RE.test(body.watch)) return { status: 400, error: "bad chat id" };
      // Only phones that can show an activity keep watches.
      if (![...sql.exec("SELECT 1 FROM la_targets WHERE device = ?", device)].length) return { status: 200 };
      const previous = this.watches(device).find((w) => w.chatId === body.watch);
      this.putWatch(device, newWatch(body.watch, this.host.loadRow("sessions", body.watch), now, previous));
      this.evaluate(device, now);
      return { status: 200 };
    }
    if (typeof body.ended === "string") {
      const current = this.activity(device);
      if (current?.activityId === body.ended) {
        sql.exec("DELETE FROM la_activities WHERE device = ?", device);
        sql.exec("DELETE FROM la_watches WHERE device = ?", device);
      }
      return { status: 200 };
    }
    return { status: 400, error: "unknown live activity registration" };
  }

  /** `DELETE /live-activity`: turned off or signed out. */
  remove(device: string, now = Date.now()): void {
    const current = this.activity(device);
    const environment = this.environment(device);
    if (current?.token) this.deliver(device, environment, current.token, { event: "end", state: current.state, dismissAt: now }, now);
    const sql = this.host.sql;
    sql.exec("DELETE FROM la_targets WHERE device = ?", device);
    sql.exec("DELETE FROM la_watches WHERE device = ?", device);
    sql.exec("DELETE FROM la_activities WHERE device = ?", device);
  }

  /** After a batch: re-plan every phone watching a touched chat. Returns the
   * `device chatId` pairs whose alert the activity carried. */
  onBatch(touched: Iterable<Row>, now = Date.now()): Set<string> {
    const chats = new Set<string>();
    for (const row of touched) if (row.kind === "sessions" || row.kind === "chats") chats.add(row.id);
    const alerted = new Set<string>();
    if (chats.size === 0) return alerted;
    const ids = [...chats];
    const devices = new Set<string>();
    for (let i = 0; i < ids.length; i += 50) {
      const page = ids.slice(i, i + 50);
      for (const raw of this.host.sql.exec<{ device: string }>(
        `SELECT DISTINCT device FROM la_watches WHERE chat_id IN (${page.map(() => "?").join(",")})`,
        ...page
      )) {
        devices.add(raw.device);
      }
    }
    for (const device of devices) {
      for (const chatId of this.evaluate(device, now)) alerted.add(`${device} ${chatId}`);
    }
    return alerted;
  }

  /** Alarm: re-plan every phone with an activity or a watch. */
  sweep(now = Date.now()): void {
    const devices = new Set<string>();
    for (const raw of this.host.sql.exec<{ device: string }>("SELECT DISTINCT device FROM la_watches")) devices.add(raw.device);
    for (const raw of this.host.sql.exec<{ device: string }>("SELECT device FROM la_activities")) devices.add(raw.device);
    for (const device of devices) this.evaluate(device, now);
  }

  stats() {
    return {
      targets: [...this.host.sql.exec<{ device: string; environment: string; start_token: string | null }>(
        "SELECT device, environment, start_token FROM la_targets"
      )].map((r) => ({ device: r.device, environment: r.environment, canStart: r.start_token !== null })),
      activities: [...this.host.sql.exec<{ device: string; activity: string }>("SELECT device, activity FROM la_activities")].map((r) => {
        const a = JSON.parse(r.activity) as ActivityRecord;
        return { device: r.device, activityId: a.activityId, confirmed: a.token !== null, startedAt: a.startedAt, state: a.state };
      }),
      watches: [...this.host.sql.exec<{ device: string; watch: string }>("SELECT device, watch FROM la_watches")].map((r) => ({
        device: r.device,
        ...(JSON.parse(r.watch) as Watch)
      })),
      log: this.log
    };
  }

  // ── internals ─────────────────────────────────────────────────────────────

  /** Plan, persist and send for one phone; the chats an alert went out for. */
  private evaluate(device: string, now: number): string[] {
    const watches = this.watches(device);
    const activity = this.activity(device);
    if (watches.length === 0 && activity === null) return [];
    const target = [...this.host.sql.exec<{ start_token: string | null }>("SELECT start_token FROM la_targets WHERE device = ?", device)][0];
    const result = plan({
      watches: watches.map((watch) => ({ watch, ctx: this.context(watch.chatId) })),
      activity,
      startToken: target?.start_token ?? null,
      prefs: this.host.prefs(device),
      now
    });
    const sql = this.host.sql;
    if (result.clear) {
      sql.exec("DELETE FROM la_watches WHERE device = ?", device);
    } else {
      for (const chatId of result.drop) sql.exec("DELETE FROM la_watches WHERE device = ? AND chat_id = ?", device, chatId);
      for (const watch of result.watches) this.putWatch(device, watch);
    }
    if (result.activity === null) sql.exec("DELETE FROM la_activities WHERE device = ?", device);
    else if (result.activity !== undefined) this.putActivity(device, result.activity);

    const environment = this.environment(device);
    const alerted: string[] = [];
    for (const send of result.sends) {
      const token = send.event === "start" ? target?.start_token : activity?.token;
      if (!token) continue;
      this.deliver(device, environment, token, send, now);
      if (send.event === "update" && send.alert) alerted.push(send.alert.chatId);
    }
    if (!result.clear || result.activity) this.host.scheduleSweep(now + SWEEP_MS);
    return alerted;
  }

  private deliver(device: string, environment: ApnsEnvironment, token: string, send: Send, now: number): void {
    const entry: LogEntry = {
      at: now,
      device,
      event: send.event,
      ...(send.event === "update" && send.alert ? { chatId: send.alert.chatId, category: send.alert.category } : {}),
      result: "pending"
    };
    this.log.push(entry);
    if (this.log.length > LOG_MAX) this.log.splice(0, this.log.length - LOG_MAX);
    const config = this.host.apns();
    if (config === undefined) {
      entry.result = "not configured";
      return;
    }
    this.host.waitUntil(
      (async () => {
        try {
          const r = await sendApns(config, environment, token, activityPayload(send, now), undefined, "liveactivity");
          entry.result = r.reason ? `${r.status} ${r.reason}` : String(r.status);
          if (!isDeadToken(r)) return;
          if (send.event === "start") {
            this.host.sql.exec("UPDATE la_targets SET start_token = NULL WHERE device = ? AND start_token = ?", device, token);
          } else {
            const current = this.activity(device);
            if (current?.token === token) this.host.sql.exec("DELETE FROM la_activities WHERE device = ?", device);
          }
        } catch (err) {
          entry.result = `error ${(err as Error).message}`;
        }
      })()
    );
  }

  private context(chatId: string): WatchContext {
    const chat = this.host.loadRow("chats", chatId);
    const session = this.host.loadRow("sessions", chatId);
    const hostId = str(chat?.fields.deviceId) ?? str(session?.fields.deviceId);
    const device = hostId ? this.host.loadRow("devices", hostId) : undefined;
    const host = device && !device.deleted ? str(device.fields.name) : undefined;
    return { chat, session, host };
  }

  private environment(device: string): ApnsEnvironment {
    const raw = [...this.host.sql.exec<{ environment: string }>("SELECT environment FROM la_targets WHERE device = ?", device)][0];
    return raw?.environment === "sandbox" ? "sandbox" : "production";
  }

  private watches(device: string): Watch[] {
    return [...this.host.sql.exec<{ watch: string }>("SELECT watch FROM la_watches WHERE device = ?", device)].map(
      (r) => JSON.parse(r.watch) as Watch
    );
  }

  private putWatch(device: string, watch: Watch): void {
    this.host.sql.exec(
      "INSERT INTO la_watches (device, chat_id, watch) VALUES (?, ?, ?) ON CONFLICT(device, chat_id) DO UPDATE SET watch = excluded.watch WHERE la_watches.watch IS NOT excluded.watch",
      device,
      watch.chatId,
      JSON.stringify(watch)
    );
  }

  private activity(device: string): ActivityRecord | null {
    const raw = [...this.host.sql.exec<{ activity: string }>("SELECT activity FROM la_activities WHERE device = ?", device)][0];
    return raw ? (JSON.parse(raw.activity) as ActivityRecord) : null;
  }

  private putActivity(device: string, activity: ActivityRecord): void {
    this.host.sql.exec(
      "INSERT INTO la_activities (device, activity) VALUES (?, ?) ON CONFLICT(device) DO UPDATE SET activity = excluded.activity WHERE la_activities.activity IS NOT excluded.activity",
      device,
      JSON.stringify(activity)
    );
  }
}
