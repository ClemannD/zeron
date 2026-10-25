/**
 * A software passkey authenticator for the self-auth tests: real ES256 keys,
 * real CBOR attestation objects (`fmt: "none"`) and DER-encoded assertion
 * signatures, so the edge's WebAuthn verification runs unmodified.
 */

const enc = new TextEncoder();

const b64u = (bytes: Uint8Array): string => {
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
};

const fromB64u = (s: string): Uint8Array => {
  const b64 = s.replaceAll("-", "+").replaceAll("_", "/");
  return Uint8Array.from(atob(b64 + "=".repeat((4 - (b64.length % 4)) % 4)), (c) => c.charCodeAt(0));
};

const concat = (...parts: Uint8Array[]): Uint8Array => {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
};

// ── minimal CBOR encoder (ints, byte/text strings, maps) ───────────────────
type Cbor = number | string | Uint8Array | Map<Cbor, Cbor> | Record<string, Cbor>;

const head = (major: number, n: number): Uint8Array => {
  if (n < 24) return Uint8Array.of((major << 5) | n);
  if (n < 0x100) return Uint8Array.of((major << 5) | 24, n);
  if (n < 0x10000) return Uint8Array.of((major << 5) | 25, n >> 8, n & 0xff);
  return Uint8Array.of((major << 5) | 26, n >>> 24, (n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff);
};

const cbor = (v: Cbor): Uint8Array => {
  if (typeof v === "number") return v >= 0 ? head(0, v) : head(1, -1 - v);
  if (typeof v === "string") {
    const bytes = enc.encode(v);
    return concat(head(3, bytes.length), bytes);
  }
  if (v instanceof Uint8Array) return concat(head(2, v.length), v);
  const entries = v instanceof Map ? [...v.entries()] : Object.entries(v);
  return concat(head(5, entries.length), ...entries.flatMap(([k, val]) => [cbor(k), cbor(val)]));
};

/** WebCrypto's raw r‖s ECDSA signature → the DER form WebAuthn carries. */
const derSignature = (raw: Uint8Array): Uint8Array => {
  const int = (bytes: Uint8Array) => {
    let i = 0;
    while (i < bytes.length - 1 && bytes[i] === 0) i++;
    let v = bytes.slice(i);
    if (v[0]! & 0x80) v = concat(Uint8Array.of(0), v);
    return concat(Uint8Array.of(0x02, v.length), v);
  };
  const body = concat(int(raw.slice(0, 32)), int(raw.slice(32)));
  return concat(Uint8Array.of(0x30, body.length), body);
};

const sha256 = async (bytes: Uint8Array) => new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));

export interface FakePasskey {
  readonly credentialId: string;
  /** Answer `navigator.credentials.create` options. */
  register(options: { challenge: string; rp: { id?: string }; user: { id: string } }, origin: string): Promise<unknown>;
  /** Answer `navigator.credentials.get` options. */
  assert(options: { challenge: string; rpId?: string }, origin: string): Promise<unknown>;
}

export const createFakePasskey = async (): Promise<FakePasskey> => {
  const keys = (await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"])) as CryptoKeyPair;
  const jwk = (await crypto.subtle.exportKey("jwk", keys.publicKey)) as JsonWebKey;
  const rawId = crypto.getRandomValues(new Uint8Array(16));
  const credentialId = b64u(rawId);
  let counter = 0;
  let userHandle = "";

  const authData = async (rpId: string, flags: number, attested?: Uint8Array) => {
    counter += 1;
    const count = Uint8Array.of(counter >>> 24, (counter >> 16) & 0xff, (counter >> 8) & 0xff, counter & 0xff);
    return concat(await sha256(enc.encode(rpId)), Uint8Array.of(flags), count, attested ?? new Uint8Array());
  };

  return {
    credentialId,
    async register(options, origin) {
      const rpId = options.rp.id ?? new URL(origin).hostname;
      userHandle = options.user.id;
      const cose = cbor(
        new Map<Cbor, Cbor>([
          [1, 2],
          [3, -7],
          [-1, 1],
          [-2, fromB64u(jwk.x!)],
          [-3, fromB64u(jwk.y!)]
        ])
      );
      const attested = concat(new Uint8Array(16), Uint8Array.of(rawId.length >> 8, rawId.length & 0xff), rawId, cose);
      // UP | UV | AT
      const data = await authData(rpId, 0x45, attested);
      const clientDataJSON = enc.encode(JSON.stringify({ type: "webauthn.create", challenge: options.challenge, origin, crossOrigin: false }));
      return {
        id: credentialId,
        rawId: credentialId,
        type: "public-key",
        clientExtensionResults: {},
        response: {
          clientDataJSON: b64u(clientDataJSON),
          attestationObject: b64u(cbor({ fmt: "none", attStmt: {}, authData: data })),
          transports: ["internal"]
        }
      };
    },
    async assert(options, origin) {
      const rpId = options.rpId ?? new URL(origin).hostname;
      // UP | UV
      const data = await authData(rpId, 0x05);
      const clientDataJSON = enc.encode(JSON.stringify({ type: "webauthn.get", challenge: options.challenge, origin, crossOrigin: false }));
      const signed = concat(data, await sha256(clientDataJSON));
      const raw = new Uint8Array(await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, keys.privateKey, signed));
      return {
        id: credentialId,
        rawId: credentialId,
        type: "public-key",
        clientExtensionResults: {},
        response: {
          clientDataJSON: b64u(clientDataJSON),
          authenticatorData: b64u(data),
          signature: b64u(derSignature(raw)),
          userHandle
        }
      };
    }
  };
};
