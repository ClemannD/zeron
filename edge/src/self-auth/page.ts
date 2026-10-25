/**
 * The hosted sign-in page (AUTH_MODE=self): passkey sign-in, plus the
 * first-run / invite form that registers a passkey. Served at
 * `/user_management/authorize` (where engines send the browser, the same
 * path WorkOS used) and `/auth/register` (invite links, no app waiting).
 *
 * Self-contained: no external scripts or styles. WebAuthn JSON ⇄ binary
 * conversion is done by hand so it works in every browser with passkeys.
 */

export interface PageContext {
  /** Where to send `code`/`state` after sign-in; absent on /auth/register. */
  readonly redirectUri?: string;
  readonly state?: string;
  /** Show the setup form first (no accounts exist yet, or an invite link). */
  readonly registerFirst: boolean;
  /** Prefilled invite code (from `/auth/register?code=`). */
  readonly inviteCode?: string;
  readonly error?: string;
}

/** JSON for embedding inside <script>: `<` escaped so nothing closes the tag. */
const scriptJson = (value: unknown): string => JSON.stringify(value).replaceAll("<", "\\u003c");

const escapeHtml = (s: string): string =>
  s
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");

export const signInPage = (ctx: PageContext): string => `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<meta name="robots" content="noindex" />
<title>Zeron — sign in</title>
<style>
  :root { color-scheme: dark; }
  body { margin: 0; min-height: 100vh; display: grid; place-items: center;
         background: #0a0a0a; color: #ededed;
         font: 15px/1.6 ui-sans-serif, system-ui, sans-serif; }
  main { width: min(22rem, 100% - 3rem); padding: 2rem 0; }
  h1 { font-size: 1.05rem; font-weight: 600; margin: 0 0 1.25rem; text-align: center; }
  p { color: #a1a1a1; margin: 0.5rem 0; font-size: 13px; text-align: center; }
  label { display: block; font-size: 12px; color: #a1a1a1; margin: 0.75rem 0 0.25rem; }
  input { box-sizing: border-box; width: 100%; padding: 0.55rem 0.7rem; border-radius: 8px;
          border: 1px solid #2e2e2e; background: #171717; color: #ededed; font: inherit; }
  button { box-sizing: border-box; width: 100%; margin-top: 1rem; padding: 0.6rem 1rem;
           border-radius: 8px; border: 1px solid #2e2e2e; background: #ededed; color: #0a0a0a;
           font: 500 14px ui-sans-serif, system-ui, sans-serif; cursor: pointer; }
  button.link { background: none; border: none; color: #a1a1a1; font-size: 13px;
                text-decoration: underline; margin-top: 0.75rem; }
  button:disabled { opacity: 0.5; cursor: default; }
  #error { color: #ff6b6b; min-height: 1.2em; }
  [hidden] { display: none !important; }
</style>
</head>
<body><main>
<section id="signin"${ctx.registerFirst ? " hidden" : ""}>
  <h1>Sign in to Zeron</h1>
  <button id="signin-btn" type="button">Sign in with passkey</button>
  <button class="link" id="to-register" type="button">Set up a passkey</button>
</section>
<section id="register"${ctx.registerFirst ? "" : " hidden"}>
  <h1>Set up a passkey</h1>
  <form id="register-form">
    <label for="email">Email</label>
    <input id="email" type="email" autocomplete="username" required />
    <label for="name">Name</label>
    <input id="name" type="text" autocomplete="name" />
    <label for="code">Setup or invite code</label>
    <input id="code" type="password" autocomplete="off" required value="${escapeHtml(ctx.inviteCode ?? "")}" />
    <button id="register-btn" type="submit">Create passkey</button>
  </form>
  <button class="link" id="to-signin" type="button">I already have a passkey</button>
</section>
<section id="done" hidden>
  <h1>All set</h1>
  <p id="done-text"></p>
</section>
<p id="error">${escapeHtml(ctx.error ?? "")}</p>
</main>
<script>
const CTX = ${scriptJson({ redirectUri: ctx.redirectUri ?? null, state: ctx.state ?? null })};
const $ = (id) => document.getElementById(id);
const b64uToBuf = (s) => {
  const b64 = s.replace(/-/g, "+").replace(/_/g, "/");
  const bin = atob(b64 + "=".repeat((4 - (b64.length % 4)) % 4));
  return Uint8Array.from(bin, (c) => c.charCodeAt(0)).buffer;
};
const bufToB64u = (buf) => {
  let bin = "";
  for (const b of new Uint8Array(buf)) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\\+/g, "-").replace(/\\//g, "_").replace(/=+$/, "");
};
const show = (id) => { for (const s of ["signin", "register", "done"]) $(s).hidden = s !== id; };
const error = (msg) => { $("error").textContent = msg || ""; };
const post = async (path, body) => {
  const res = await fetch(path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || "request failed (" + res.status + ")");
  return data;
};
const finish = (data, fallback) => {
  if (data.redirect) { location.href = data.redirect; return; }
  show("done");
  $("done-text").textContent = fallback;
};
const busy = (btn, on) => { btn.disabled = on; };

$("to-register").onclick = () => { error(); show("register"); };
$("to-signin").onclick = () => { error(); show("signin"); };

$("signin-btn").onclick = async () => {
  const btn = $("signin-btn");
  error(); busy(btn, true);
  try {
    if (!window.PublicKeyCredential) throw new Error("This browser does not support passkeys.");
    const { challengeId, options } = await post("/auth/passkey/login/options", {});
    const cred = await navigator.credentials.get({ publicKey: {
      challenge: b64uToBuf(options.challenge),
      rpId: options.rpId,
      timeout: options.timeout,
      userVerification: options.userVerification,
      allowCredentials: (options.allowCredentials || []).map((c) => ({ ...c, id: b64uToBuf(c.id) }))
    } });
    const r = cred.response;
    const response = {
      id: cred.id, rawId: bufToB64u(cred.rawId), type: cred.type,
      authenticatorAttachment: cred.authenticatorAttachment || undefined,
      clientExtensionResults: cred.getClientExtensionResults(),
      response: {
        clientDataJSON: bufToB64u(r.clientDataJSON),
        authenticatorData: bufToB64u(r.authenticatorData),
        signature: bufToB64u(r.signature),
        userHandle: r.userHandle ? bufToB64u(r.userHandle) : undefined
      }
    };
    const data = await post("/auth/passkey/login/verify", { challengeId, response, redirectUri: CTX.redirectUri, state: CTX.state });
    finish(data, "You're signed in. Return to Zeron.");
  } catch (e) {
    error(e.name === "NotAllowedError" ? "Sign-in was cancelled." : e.message);
  } finally { busy(btn, false); }
};

$("register-form").onsubmit = async (ev) => {
  ev.preventDefault();
  const btn = $("register-btn");
  error(); busy(btn, true);
  try {
    if (!window.PublicKeyCredential) throw new Error("This browser does not support passkeys.");
    const { challengeId, options } = await post("/auth/passkey/register/options", {
      email: $("email").value, name: $("name").value, code: $("code").value
    });
    const cred = await navigator.credentials.create({ publicKey: {
      ...options,
      challenge: b64uToBuf(options.challenge),
      user: { ...options.user, id: b64uToBuf(options.user.id) },
      excludeCredentials: (options.excludeCredentials || []).map((c) => ({ ...c, id: b64uToBuf(c.id) }))
    } });
    const r = cred.response;
    const response = {
      id: cred.id, rawId: bufToB64u(cred.rawId), type: cred.type,
      authenticatorAttachment: cred.authenticatorAttachment || undefined,
      clientExtensionResults: cred.getClientExtensionResults(),
      response: {
        clientDataJSON: bufToB64u(r.clientDataJSON),
        attestationObject: bufToB64u(r.attestationObject),
        transports: typeof r.getTransports === "function" ? r.getTransports() : undefined
      }
    };
    const data = await post("/auth/passkey/register/verify", { challengeId, response, redirectUri: CTX.redirectUri, state: CTX.state });
    finish(data, "Your passkey is ready. Open Zeron and sign in.");
  } catch (e) {
    error(e.name === "NotAllowedError" ? "Passkey creation was cancelled." : e.name === "InvalidStateError" ? "This passkey is already registered — sign in instead." : e.message);
  } finally { busy(btn, false); }
};
</script>
</body>
</html>`;
