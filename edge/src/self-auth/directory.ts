/**
 * AuthDirectory — ONE Durable Object (`auth1`) that is this deployment's
 * identity provider under AUTH_MODE=self: users, their passkeys (WebAuthn),
 * organizations and memberships, sign-in codes, invites, and sessions with
 * rotating single-use refresh tokens. Replaces WorkOS User Management.
 *
 * Sign-up is closed. The first account is created with the `AUTH_SETUP_CODE`
 * secret (accepted only while there are no users); everyone after that —
 * including a second passkey for an existing user — needs an invite minted
 * with the operator ADMIN_TOKEN (`POST /admin/auth/invites`).
 *
 * Refresh rotation: every refresh consumes its token and issues a successor.
 * Presenting a consumed token again within REUSE_GRACE_MS (a response lost in
 * flight, a retry) re-issues from it and invalidates the earlier successor;
 * any later reuse is treated as theft and revokes the whole session.
 *
 * Writes are tiny and rare (sign-ins, refreshes every ~10 min per device),
 * so a single instance is not a hotspot for a personal deployment.
 */
import { DurableObject } from "cloudflare:workers";
import {
  generateAuthenticationOptions,
  generateRegistrationOptions,
  verifyAuthenticationResponse,
  verifyRegistrationResponse,
  type AuthenticationResponseJSON,
  type PublicKeyCredentialCreationOptionsJSON,
  type PublicKeyCredentialRequestOptionsJSON,
  type RegistrationResponseJSON
} from "@simplewebauthn/server";
import type { Env } from "../env";
import { mintAccessToken, randomId, randomToken, secretEquals, sha256 } from "./tokens";

export const AUTH_DIRECTORY_NAME = "auth1";

const CHALLENGE_TTL_MS = 5 * 60 * 1000;
/** Long enough to copy a headless paste-code into a terminal. */
const CODE_TTL_MS = 5 * 60 * 1000;
const SESSION_IDLE_MS = 90 * 24 * 60 * 60 * 1000;
const REUSE_GRACE_MS = 60 * 1000;
const DEFAULT_INVITE_TTL_MS = 7 * 24 * 60 * 60 * 1000;
const RP_NAME = "Zeron";

export type Result<T> = { ok: true; value: T } | { ok: false; status: number; error: string };

const ok = <T>(value: T): Result<T> => ({ ok: true, value });
const fail = (status: number, error: string): Result<never> => ({ ok: false, status, error });

/** The relying party a ceremony runs against: the host the page was served from. */
export interface RelyingParty {
  readonly rpID: string;
  readonly origin: string;
}

export interface User {
  readonly id: string;
  readonly email: string;
  readonly firstName: string | null;
  readonly lastName: string | null;
}

export interface Membership {
  readonly id: string;
  readonly organizationId: string;
  readonly name: string;
}

export interface Tokens {
  readonly accessToken: string;
  readonly refreshToken: string;
}

interface RegisterPayload {
  readonly email: string;
  readonly name: string | null;
  /** Existing user (invite for a second passkey) or the id to create. */
  readonly userId: string;
  readonly existing: boolean;
  readonly inviteHash: string | null;
  readonly inviteOrgId: string | null;
}

const EMAIL_RE = /^[^\s@]{1,64}@[^\s@]{1,190}$/;

const splitName = (name: string | null): { firstName: string | null; lastName: string | null } => {
  const trimmed = name?.trim();
  if (!trimmed) return { firstName: null, lastName: null };
  const at = trimmed.indexOf(" ");
  return at < 0
    ? { firstName: trimmed, lastName: null }
    : { firstName: trimmed.slice(0, at), lastName: trimmed.slice(at + 1).trim() || null };
};

const bytesToBase64Url = (bytes: Uint8Array): string => {
  let binary = "";
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
};

const base64UrlToBytes = (value: string): Uint8Array<ArrayBuffer> => {
  const b64 = value.replaceAll("-", "+").replaceAll("_", "/");
  const binary = atob(b64 + "=".repeat((4 - (b64.length % 4)) % 4));
  const out = new Uint8Array(new ArrayBuffer(binary.length));
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
};

/** UTF-8 bytes on a plain ArrayBuffer (WebAuthn's `user.id`). */
const utf8 = (value: string): Uint8Array<ArrayBuffer> => {
  const encoded = new TextEncoder().encode(value);
  const out = new Uint8Array(new ArrayBuffer(encoded.length));
  out.set(encoded);
  return out;
};

export class AuthDirectory extends DurableObject<Env> {
  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    const sql = ctx.storage.sql;
    sql.exec(`CREATE TABLE IF NOT EXISTS users (
      id TEXT PRIMARY KEY, email TEXT NOT NULL UNIQUE, first_name TEXT, last_name TEXT,
      created_at INTEGER NOT NULL)`);
    sql.exec(`CREATE TABLE IF NOT EXISTS credentials (
      id TEXT PRIMARY KEY, user_id TEXT NOT NULL, public_key TEXT NOT NULL, counter INTEGER NOT NULL,
      transports TEXT, created_at INTEGER NOT NULL, last_used_at INTEGER)`);
    sql.exec(`CREATE TABLE IF NOT EXISTS orgs (id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at INTEGER NOT NULL)`);
    sql.exec(`CREATE TABLE IF NOT EXISTS memberships (
      id TEXT PRIMARY KEY, user_id TEXT NOT NULL, org_id TEXT NOT NULL, role TEXT NOT NULL,
      created_at INTEGER NOT NULL, UNIQUE (user_id, org_id))`);
    sql.exec(`CREATE TABLE IF NOT EXISTS challenges (
      id TEXT PRIMARY KEY, kind TEXT NOT NULL, challenge TEXT NOT NULL, payload TEXT,
      expires_at INTEGER NOT NULL)`);
    sql.exec(`CREATE TABLE IF NOT EXISTS codes (
      hash TEXT PRIMARY KEY, user_id TEXT NOT NULL, expires_at INTEGER NOT NULL)`);
    sql.exec(`CREATE TABLE IF NOT EXISTS invites (
      hash TEXT PRIMARY KEY, email TEXT NOT NULL, org_id TEXT, created_at INTEGER NOT NULL,
      expires_at INTEGER NOT NULL, used_at INTEGER)`);
    sql.exec(`CREATE TABLE IF NOT EXISTS sessions (
      id TEXT PRIMARY KEY, user_id TEXT NOT NULL, org_id TEXT, created_at INTEGER NOT NULL,
      last_used_at INTEGER NOT NULL, revoked_at INTEGER)`);
    sql.exec(`CREATE TABLE IF NOT EXISTS refresh_tokens (
      hash TEXT PRIMARY KEY, session_id TEXT NOT NULL, created_at INTEGER NOT NULL,
      used_at INTEGER, successor TEXT)`);
  }

  private get sql() {
    return this.ctx.storage.sql;
  }

  private prune(now: number): void {
    this.sql.exec("DELETE FROM challenges WHERE expires_at < ?", now);
    this.sql.exec("DELETE FROM codes WHERE expires_at < ?", now);
  }

  private userCount(): number {
    return this.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM users").one().n;
  }

  private user(id: string): User | undefined {
    const row = this.sql
      .exec<{ id: string; email: string; first_name: string | null; last_name: string | null }>(
        "SELECT id, email, first_name, last_name FROM users WHERE id = ?",
        id
      )
      .toArray()[0];
    return row && { id: row.id, email: row.email, firstName: row.first_name, lastName: row.last_name };
  }

  private takeChallenge(id: string, kind: string): { challenge: string; payload: string | null } | undefined {
    const row = this.sql
      .exec<{ challenge: string; payload: string | null; expires_at: number }>(
        "SELECT challenge, payload, expires_at FROM challenges WHERE id = ? AND kind = ?",
        id,
        kind
      )
      .toArray()[0];
    this.sql.exec("DELETE FROM challenges WHERE id = ?", id);
    if (!row || row.expires_at < Date.now()) return undefined;
    return row;
  }

  private async mintCode(userId: string, now: number): Promise<string> {
    const code = randomToken();
    this.sql.exec("INSERT INTO codes (hash, user_id, expires_at) VALUES (?, ?, ?)", await sha256(code), userId, now + CODE_TTL_MS);
    return code;
  }

  private signingKey(): string | undefined {
    return this.env.AUTH_JWT_PRIVATE_KEY;
  }

  private async issueRefresh(sessionId: string, now: number): Promise<{ token: string; hash: string }> {
    const token = randomToken();
    const hash = await sha256(token);
    this.sql.exec("INSERT INTO refresh_tokens (hash, session_id, created_at) VALUES (?, ?, ?)", hash, sessionId, now);
    return { token, hash };
  }

  private revokeSession(sessionId: string, now: number): void {
    this.sql.exec("UPDATE sessions SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL", now, sessionId);
    this.sql.exec("DELETE FROM refresh_tokens WHERE session_id = ?", sessionId);
  }

  // ── passkey sign-in ─────────────────────────────────────────────────────

  async loginOptions(rp: RelyingParty): Promise<Result<{ challengeId: string; options: PublicKeyCredentialRequestOptionsJSON }>> {
    const now = Date.now();
    this.prune(now);
    // Discoverable credentials: the authenticator picks the account, so the
    // page never asks for (or reveals whether we know) an email.
    const options = await generateAuthenticationOptions({ rpID: rp.rpID, userVerification: "preferred" });
    const challengeId = randomToken(18);
    this.sql.exec(
      "INSERT INTO challenges (id, kind, challenge, payload, expires_at) VALUES (?, 'login', ?, NULL, ?)",
      challengeId,
      options.challenge,
      now + CHALLENGE_TTL_MS
    );
    return ok({ challengeId, options });
  }

  /** Verify a passkey assertion; on success mint a one-time sign-in code. */
  async loginVerify(
    rp: RelyingParty,
    challengeId: string,
    response: AuthenticationResponseJSON
  ): Promise<Result<{ code: string }>> {
    const pending = this.takeChallenge(challengeId, "login");
    if (!pending) return fail(400, "sign-in expired — try again");
    const row = this.sql
      .exec<{ id: string; user_id: string; public_key: string; counter: number; transports: string | null }>(
        "SELECT id, user_id, public_key, counter, transports FROM credentials WHERE id = ?",
        response.id
      )
      .toArray()[0];
    if (!row) return fail(401, "this passkey is not registered here");
    let verified;
    try {
      verified = await verifyAuthenticationResponse({
        response,
        expectedChallenge: pending.challenge,
        expectedOrigin: rp.origin,
        expectedRPID: rp.rpID,
        credential: {
          id: row.id,
          publicKey: base64UrlToBytes(row.public_key),
          counter: row.counter,
          ...(row.transports ? { transports: JSON.parse(row.transports) as string[] } : {})
        },
        requireUserVerification: false
      });
    } catch (e) {
      return fail(401, e instanceof Error ? e.message : "passkey verification failed");
    }
    if (!verified.verified) return fail(401, "passkey verification failed");
    const now = Date.now();
    this.sql.exec(
      "UPDATE credentials SET counter = ?, last_used_at = ? WHERE id = ?",
      verified.authenticationInfo.newCounter,
      now,
      row.id
    );
    return ok({ code: await this.mintCode(row.user_id, now) });
  }

  // ── passkey registration (setup code or invite) ─────────────────────────

  async registerOptions(
    rp: RelyingParty,
    request: { code: string; email: string; name?: string }
  ): Promise<Result<{ challengeId: string; options: PublicKeyCredentialCreationOptionsJSON }>> {
    const now = Date.now();
    this.prune(now);
    const email = request.email.trim().toLowerCase();
    if (!EMAIL_RE.test(email)) return fail(400, "enter a valid email");
    const name = request.name?.trim().slice(0, 80) || null;
    const code = request.code.trim();
    if (!code) return fail(400, "enter your setup code or invite code");

    let inviteHash: string | null = null;
    let inviteOrgId: string | null = null;
    const setup = this.env.AUTH_SETUP_CODE;
    if (this.userCount() === 0 && setup && (await secretEquals(code, setup))) {
      // First account: the setup code, valid only while nobody exists.
    } else {
      const hash = await sha256(code);
      const invite = this.sql
        .exec<{ email: string; org_id: string | null; expires_at: number; used_at: number | null }>(
          "SELECT email, org_id, expires_at, used_at FROM invites WHERE hash = ?",
          hash
        )
        .toArray()[0];
      if (!invite || invite.used_at !== null || invite.expires_at < now) {
        return fail(403, "that code is not valid (it may be used or expired)");
      }
      if (invite.email !== email) return fail(403, "this invite is for a different email");
      inviteHash = hash;
      inviteOrgId = invite.org_id;
    }

    const existing = this.sql
      .exec<{ id: string }>("SELECT id FROM users WHERE email = ?", email)
      .toArray()[0];
    const userId = existing?.id ?? randomId("user");
    const exclude = existing
      ? this.sql
          .exec<{ id: string; transports: string | null }>("SELECT id, transports FROM credentials WHERE user_id = ?", existing.id)
          .toArray()
          .map((c) => ({ id: c.id, ...(c.transports ? { transports: JSON.parse(c.transports) as string[] } : {}) }))
      : [];
    const options = await generateRegistrationOptions({
      rpName: RP_NAME,
      rpID: rp.rpID,
      userName: email,
      userID: utf8(userId),
      userDisplayName: name ?? email,
      attestationType: "none",
      excludeCredentials: exclude,
      authenticatorSelection: { residentKey: "required", userVerification: "preferred" }
    });
    const payload: RegisterPayload = { email, name, userId, existing: !!existing, inviteHash, inviteOrgId };
    const challengeId = randomToken(18);
    this.sql.exec(
      "INSERT INTO challenges (id, kind, challenge, payload, expires_at) VALUES (?, 'register', ?, ?, ?)",
      challengeId,
      options.challenge,
      JSON.stringify(payload),
      now + CHALLENGE_TTL_MS
    );
    return ok({ challengeId, options });
  }

  /** Verify a new passkey, create the account (and its first org) if new,
   * and mint a one-time sign-in code so the page can continue straight on. */
  async registerVerify(
    rp: RelyingParty,
    challengeId: string,
    response: RegistrationResponseJSON
  ): Promise<Result<{ code: string; userId: string }>> {
    const pending = this.takeChallenge(challengeId, "register");
    if (!pending?.payload) return fail(400, "setup expired — try again");
    const p = JSON.parse(pending.payload) as RegisterPayload;
    let verified;
    try {
      verified = await verifyRegistrationResponse({
        response,
        expectedChallenge: pending.challenge,
        expectedOrigin: rp.origin,
        expectedRPID: rp.rpID,
        requireUserVerification: false
      });
    } catch (e) {
      return fail(400, e instanceof Error ? e.message : "passkey registration failed");
    }
    if (!verified.verified) return fail(400, "passkey registration failed");
    const now = Date.now();

    // Re-check the grant at commit time: the setup code only while nobody
    // exists (two tabs racing), an invite only once.
    if (p.inviteHash === null) {
      if (this.userCount() !== 0) return fail(403, "this deployment already has an account — ask for an invite");
    } else {
      const used = this.sql
        .exec<{ used_at: number | null }>("SELECT used_at FROM invites WHERE hash = ?", p.inviteHash)
        .toArray()[0];
      if (!used || used.used_at !== null) return fail(403, "that invite was already used");
      this.sql.exec("UPDATE invites SET used_at = ? WHERE hash = ?", now, p.inviteHash);
    }

    if (!p.existing) {
      const { firstName, lastName } = splitName(p.name);
      this.sql.exec(
        "INSERT INTO users (id, email, first_name, last_name, created_at) VALUES (?, ?, ?, ?, ?)",
        p.userId,
        p.email,
        firstName,
        lastName,
        now
      );
      const orgId = p.inviteOrgId && this.orgExists(p.inviteOrgId) ? p.inviteOrgId : this.createOrgRow(`${firstName ?? p.email.split("@")[0]}'s workspace`, now);
      this.addMembership(p.userId, orgId, p.inviteOrgId === orgId ? "member" : "admin", now);
    } else if (p.inviteOrgId && this.orgExists(p.inviteOrgId)) {
      this.addMembership(p.userId, p.inviteOrgId, "member", now);
    }

    const cred = verified.registrationInfo.credential;
    this.sql.exec(
      "INSERT INTO credentials (id, user_id, public_key, counter, transports, created_at) VALUES (?, ?, ?, ?, ?, ?)",
      cred.id,
      p.userId,
      bytesToBase64Url(cred.publicKey),
      cred.counter,
      cred.transports ? JSON.stringify(cred.transports) : null,
      now
    );
    return ok({ code: await this.mintCode(p.userId, now), userId: p.userId });
  }

  // ── token endpoints (WorkOS-shaped, called by /auth/exchange|refresh) ────

  /** Sign-in code → user + tokens. The session starts scoped to the user's
   * first org, so a fresh device lands in its workspace without onboarding. */
  async exchange(code: string): Promise<Result<{ user: User } & Tokens>> {
    const key = this.signingKey();
    if (!key) return fail(501, "auth not configured");
    const now = Date.now();
    const hash = await sha256(code);
    const row = this.sql
      .exec<{ user_id: string; expires_at: number }>("SELECT user_id, expires_at FROM codes WHERE hash = ?", hash)
      .toArray()[0];
    this.sql.exec("DELETE FROM codes WHERE hash = ?", hash);
    if (!row || row.expires_at < now) return fail(401, "invalid or expired code");
    const user = this.user(row.user_id);
    if (!user) return fail(401, "invalid or expired code");
    const orgId =
      this.sql
        .exec<{ org_id: string }>("SELECT org_id FROM memberships WHERE user_id = ? ORDER BY created_at LIMIT 1", user.id)
        .toArray()[0]?.org_id ?? null;
    const sessionId = randomId("session");
    this.sql.exec(
      "INSERT INTO sessions (id, user_id, org_id, created_at, last_used_at) VALUES (?, ?, ?, ?, ?)",
      sessionId,
      user.id,
      orgId,
      now,
      now
    );
    const refresh = await this.issueRefresh(sessionId, now);
    const accessToken = await mintAccessToken(key, { userId: user.id, sessionId, ...(orgId ? { orgId } : {}) }, now);
    return ok({ user, accessToken, refreshToken: refresh.token });
  }

  async refresh(refreshToken: string, organizationId?: string): Promise<Result<Tokens>> {
    const key = this.signingKey();
    if (!key) return fail(501, "auth not configured");
    const now = Date.now();
    const hash = await sha256(refreshToken);
    const token = this.sql
      .exec<{ session_id: string; used_at: number | null; successor: string | null }>(
        "SELECT session_id, used_at, successor FROM refresh_tokens WHERE hash = ?",
        hash
      )
      .toArray()[0];
    if (!token) return fail(401, "invalid refresh token");
    const session = this.sql
      .exec<{ user_id: string; org_id: string | null; last_used_at: number; revoked_at: number | null }>(
        "SELECT user_id, org_id, last_used_at, revoked_at FROM sessions WHERE id = ?",
        token.session_id
      )
      .toArray()[0];
    if (!session || session.revoked_at !== null) return fail(401, "session revoked");
    if (now - session.last_used_at > SESSION_IDLE_MS) {
      this.revokeSession(token.session_id, now);
      return fail(401, "session expired — sign in again");
    }

    if (token.used_at !== null) {
      // Reuse. Within the grace window and only for the newest consumed
      // token (its successor unused): a lost response — re-issue from it.
      const successor = token.successor
        ? this.sql
            .exec<{ used_at: number | null }>("SELECT used_at FROM refresh_tokens WHERE hash = ?", token.successor)
            .toArray()[0]
        : undefined;
      const retry = now - token.used_at <= REUSE_GRACE_MS && (!successor || successor.used_at === null);
      if (!retry) {
        console.warn("auth: refresh token reuse; revoking session", token.session_id);
        this.revokeSession(token.session_id, now);
        return fail(401, "refresh token already used — sign in again");
      }
      if (token.successor) this.sql.exec("DELETE FROM refresh_tokens WHERE hash = ?", token.successor);
    }

    let orgId = session.org_id;
    if (organizationId !== undefined) {
      const member = this.sql
        .exec("SELECT 1 FROM memberships WHERE user_id = ? AND org_id = ?", session.user_id, organizationId)
        .toArray()[0];
      if (!member) return fail(401, "not a member of that organization");
      orgId = organizationId;
    }
    const next = await this.issueRefresh(token.session_id, now);
    this.sql.exec("UPDATE refresh_tokens SET used_at = COALESCE(used_at, ?), successor = ? WHERE hash = ?", now, next.hash, hash);
    this.sql.exec("UPDATE sessions SET org_id = ?, last_used_at = ? WHERE id = ?", orgId, now, token.session_id);
    const accessToken = await mintAccessToken(
      key,
      { userId: session.user_id, sessionId: token.session_id, ...(orgId ? { orgId } : {}) },
      now
    );
    return ok({ accessToken, refreshToken: next.token });
  }

  // ── organizations ───────────────────────────────────────────────────────

  private orgExists(orgId: string): boolean {
    return this.sql.exec("SELECT 1 FROM orgs WHERE id = ?", orgId).toArray().length > 0;
  }

  private createOrgRow(name: string, now: number): string {
    const id = randomId("org");
    this.sql.exec("INSERT INTO orgs (id, name, created_at) VALUES (?, ?, ?)", id, name, now);
    return id;
  }

  private addMembership(userId: string, orgId: string, role: string, now: number): void {
    this.sql.exec(
      "INSERT OR IGNORE INTO memberships (id, user_id, org_id, role, created_at) VALUES (?, ?, ?, ?, ?)",
      randomId("om"),
      userId,
      orgId,
      role,
      now
    );
  }

  listOrgs(userId: string): Membership[] {
    return this.sql
      .exec<{ id: string; org_id: string; name: string }>(
        `SELECT m.id, m.org_id, o.name FROM memberships m JOIN orgs o ON o.id = m.org_id
         WHERE m.user_id = ? ORDER BY m.created_at`,
        userId
      )
      .toArray()
      .map((r) => ({ id: r.id, organizationId: r.org_id, name: r.name }));
  }

  createOrg(userId: string, name: string): Result<{ organizationId: string }> {
    if (!this.user(userId)) return fail(401, "unknown user");
    const now = Date.now();
    const organizationId = this.createOrgRow(name, now);
    this.addMembership(userId, organizationId, "admin", now);
    return ok({ organizationId });
  }

  // ── operator ────────────────────────────────────────────────────────────

  async createInvite(request: { email: string; orgId?: string; ttlMs?: number }): Promise<Result<{ code: string; expiresAt: number }>> {
    const email = request.email.trim().toLowerCase();
    if (!EMAIL_RE.test(email)) return fail(400, "invalid email");
    if (request.orgId !== undefined && !this.orgExists(request.orgId)) return fail(404, "unknown org");
    const now = Date.now();
    const code = randomToken();
    const expiresAt = now + Math.min(request.ttlMs ?? DEFAULT_INVITE_TTL_MS, 30 * 24 * 60 * 60 * 1000);
    this.sql.exec(
      "INSERT INTO invites (hash, email, org_id, created_at, expires_at) VALUES (?, ?, ?, ?, ?)",
      await sha256(code),
      email,
      request.orgId ?? null,
      now,
      expiresAt
    );
    return ok({ code, expiresAt });
  }

  listUsers(): { user: User; passkeys: number; orgs: Membership[]; activeSessions: number }[] {
    return this.sql
      .exec<{ id: string }>("SELECT id FROM users ORDER BY created_at")
      .toArray()
      .map(({ id }) => ({
        user: this.user(id)!,
        passkeys: this.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM credentials WHERE user_id = ?", id).one().n,
        orgs: this.listOrgs(id),
        activeSessions: this.sql
          .exec<{ n: number }>("SELECT COUNT(*) AS n FROM sessions WHERE user_id = ? AND revoked_at IS NULL", id)
          .one().n
      }));
  }

  /** Sign a user out everywhere: every session's refresh chain dies now,
   * outstanding access tokens within their 10-minute lifetime. */
  revokeUser(userId: string): { revoked: number } {
    const now = Date.now();
    const ids = this.sql
      .exec<{ id: string }>("SELECT id FROM sessions WHERE user_id = ? AND revoked_at IS NULL", userId)
      .toArray();
    for (const { id } of ids) this.revokeSession(id, now);
    return { revoked: ids.length };
  }

  /** Whether the first-run setup form should be offered. */
  needsSetup(): boolean {
    return this.userCount() === 0 && !!this.env.AUTH_SETUP_CODE;
  }
}
