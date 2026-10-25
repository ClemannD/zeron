/**
 * Self-hosted auth tokens (AUTH_MODE=self): the edge is its own identity
 * provider in place of WorkOS. Access tokens are ES256 JWTs signed with
 * `AUTH_JWT_PRIVATE_KEY` (a private JWK; the public half is derived from it)
 * and carry the claims the engine and edge already read from WorkOS tokens:
 * `sub`, `sid`, `org_id`, `exp`, `iat`. Refresh tokens, sign-in codes and
 * invites are opaque random strings stored only as SHA-256 hashes.
 */
import { SignJWT, decodeJwt, importJWK, jwtVerify, type JWK } from "jose";

export const SELF_ISSUER = "zeron-auth";
export const ACCESS_TOKEN_TTL_S = 10 * 60;

export interface AccessClaims {
  readonly userId: string;
  readonly sessionId: string;
  readonly orgId?: string;
}

interface Keys {
  readonly privateKey: CryptoKey | Uint8Array;
  readonly publicKey: CryptoKey | Uint8Array;
  readonly kid?: string;
}

// Imported keys are cached per secret value (importJWK is not free; the
// secret only changes on redeploy, which drops the isolate anyway).
const keyCache = new Map<string, Promise<Keys>>();

const importKeys = (jwkJson: string): Promise<Keys> => {
  let cached = keyCache.get(jwkJson);
  if (!cached) {
    cached = (async () => {
      const jwk = JSON.parse(jwkJson) as JWK;
      const { d: _d, ...publicJwk } = jwk;
      return {
        privateKey: await importJWK(jwk, "ES256"),
        publicKey: await importJWK(publicJwk, "ES256"),
        kid: jwk.kid
      };
    })();
    // A malformed secret must not poison the cache forever.
    cached.catch(() => keyCache.delete(jwkJson));
    keyCache.set(jwkJson, cached);
  }
  return cached;
};

export const mintAccessToken = async (
  privateJwk: string,
  claims: AccessClaims,
  now = Date.now()
): Promise<string> => {
  const { privateKey, kid } = await importKeys(privateJwk);
  const iat = Math.floor(now / 1000);
  return new SignJWT({ sid: claims.sessionId, ...(claims.orgId ? { org_id: claims.orgId } : {}) })
    .setProtectedHeader({ alg: "ES256", ...(kid ? { kid } : {}) })
    .setIssuer(SELF_ISSUER)
    .setSubject(claims.userId)
    .setIssuedAt(iat)
    .setExpirationTime(iat + ACCESS_TOKEN_TTL_S)
    .sign(privateKey);
};

/** `undefined` for anything that is not a live token we signed. */
export const verifyAccessToken = async (
  privateJwk: string,
  token: string
): Promise<AccessClaims | undefined> => {
  try {
    const { publicKey } = await importKeys(privateJwk);
    const { payload } = await jwtVerify(token, publicKey, { issuer: SELF_ISSUER, algorithms: ["ES256"] });
    if (typeof payload.sub !== "string" || payload.sub.length === 0) return undefined;
    if (typeof payload.sid !== "string" || payload.sid.length === 0) return undefined;
    return {
      userId: payload.sub,
      sessionId: payload.sid,
      ...(typeof payload.org_id === "string" ? { orgId: payload.org_id } : {})
    };
  } catch {
    return undefined;
  }
};

/** Cheap pre-check (no signature work): does this token claim our issuer? */
export const claimsSelfIssuer = (token: string): boolean => {
  try {
    return decodeJwt(token).iss === SELF_ISSUER;
  } catch {
    return false;
  }
};

const toBase64Url = (bytes: Uint8Array): string => {
  let binary = "";
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
};

/** A fresh opaque secret: 32 random bytes, base64url. */
export const randomToken = (bytes = 32): string => toBase64Url(crypto.getRandomValues(new Uint8Array(bytes)));

/** Prefixed id that fits the edge's ID_RE (`[A-Za-z0-9_-]`). */
export const randomId = (prefix: string): string => `${prefix}_${crypto.randomUUID().replaceAll("-", "")}`;

export const sha256 = async (value: string): Promise<string> =>
  toBase64Url(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value))));

/** Constant-time string comparison (both sides hashed first, so length leaks nothing). */
export const secretEquals = async (a: string, b: string): Promise<boolean> => {
  const [x, y] = await Promise.all([sha256(a), sha256(b)]);
  let diff = 0;
  for (let i = 0; i < x.length; i++) diff |= x.charCodeAt(i) ^ y.charCodeAt(i);
  return diff === 0 && x.length === y.length;
};
