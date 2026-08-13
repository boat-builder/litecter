/**
 * Litecter sync backend — a blind blob store that you run.
 *
 * This file is the whole backend. There is no build step and no dependency:
 * what you are reading is exactly what gets deployed, which is the point. You
 * are being asked to paste it into your own Cloudflare account, so it stays
 * short enough to read first.
 *
 * It holds one R2 object: your watch list, sealed on your machine with a key
 * this Worker never receives. It cannot read a watch list even if it wanted to
 * — it sees an opaque byte string and a bearer token.
 *
 * Concurrency is optimistic: GET returns an ETag, PUT requires it back via
 * If-Match. A second device that raced you gets 412, re-pulls, re-merges and
 * retries.
 *
 * Setup, update and teardown: see the README next to this file, or
 * https://github.com/boat-builder/litecter/blob/main/worker/README.md
 */

/**
 * Version log. Bump this on *every* change that needs to reach deployments,
 * including ones invisible to the API — the number is the rollout mechanism.
 * Litecter ships a copy of this file, reads the constant back out of it, and
 * tells you when what you deployed is older than what your app expects.
 *
 *   1 — first versioned worker. Adds GET /v1/meta and requires the SYNC_TOKEN
 *       secret. (Pre-1 workers had no /v1/meta and accepted any well-formed
 *       token, which was safe only because nobody knew the URL.)
 */
const WORKER_VERSION = 1;

/**
 * What this deployment can do. The version answers "is it behind"; this
 * answers "can it do X" without the app having to memorise version history.
 */
const WORKER_FEATURES = ['blob', 'meta'];

/** Sealed docs are tiny (~100 KB typical). This is an abuse ceiling, not a target. */
const MAX_BLOB_BYTES = 8 * 1024 * 1024;

/** The client derives its token with blake3 and renders it as lowercase hex. */
const TOKEN_RE = /^[0-9a-f]{64}$/;

/**
 * @typedef {Object} Env
 * @property {R2Bucket} SYNC - the bucket bound in wrangler.toml
 * @property {string} [SYNC_TOKEN] - `wrangler secret put SYNC_TOKEN`
 */

/**
 * @param {unknown} body
 * @param {number} [status]
 * @param {HeadersInit} [headers]
 */
const json = (body, status = 200, headers = {}) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json', ...headers },
  });

/**
 * @param {number} status
 * @param {string} message
 */
const err = (status, message) => json({ error: message }, status);

/**
 * Storage path for a token. Hashing here — rather than trusting a
 * client-supplied path — is what stops one token addressing another's object,
 * and keeps the token itself out of your bucket listing.
 *
 * @param {string} token
 * @returns {Promise<string>}
 */
async function storageKey(token) {
  const data = new TextEncoder().encode(`litecter-sync-v1:${token}`);
  const digest = await crypto.subtle.digest('SHA-256', data);
  const hex = [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
  return `blobs/${hex}`;
}

/**
 * Length-independent comparison. Both sides are hex of a fixed length, so this
 * is belt-and-braces rather than load-bearing — but a timing oracle on the one
 * secret this service holds is not a thing to leave lying around.
 *
 * @param {string} a
 * @param {string} b
 */
function constantTimeEqual(a, b) {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return diff === 0;
}

/**
 * Pull the bearer token off a request. Normalised, because the far end of this
 * comparison is a secret someone pasted into a terminal — a trailing newline
 * silently breaking every sync is a bad way to spend an evening.
 *
 * @param {Request} request
 * @returns {string | null}
 */
function bearer(request) {
  const header = request.headers.get('authorization') ?? '';
  const match = /^Bearer\s+(.+)$/i.exec(header.trim());
  if (!match) return null;
  const token = match[1].trim().toLowerCase();
  return TOKEN_RE.test(token) ? token : null;
}

/** R2 reports ETags both bare and quoted; conditionals want the bare form. */
const unquote = (/** @type {string} */ etag) =>
  etag.trim().replace(/^W\//, '').replace(/^"|"$/g, '');

/**
 * @param {Env} env
 * @param {string} key
 */
async function handleGet(env, key) {
  const object = await env.SYNC.get(key);
  if (!object) return err(404, 'no document for this key');
  return new Response(object.body, {
    headers: {
      'content-type': 'application/octet-stream',
      etag: `"${object.etag}"`,
      'cache-control': 'no-store',
    },
  });
}

/**
 * @param {Request} request
 * @param {Env} env
 * @param {string} key
 */
async function handlePut(request, env, key) {
  const declared = Number(request.headers.get('content-length') ?? '0');
  if (declared > MAX_BLOB_BYTES) return err(413, 'document too large');

  const body = new Uint8Array(await request.arrayBuffer());
  if (body.byteLength === 0) return err(400, 'empty body');
  if (body.byteLength > MAX_BLOB_BYTES) return err(413, 'document too large');

  const ifMatch = request.headers.get('if-match');

  // No If-Match means "I saw no document when I pulled". Two brand-new devices
  // can both land here and the second wins — self-healing, because the loser
  // pulls the winner's doc on its next sync and merges into it.
  const options = ifMatch ? { onlyIf: { etagMatches: unquote(ifMatch) } } : undefined;

  const written = await env.SYNC.put(key, body, options);
  if (!written) return err(412, 'document changed since you pulled it');

  return json({ etag: written.etag }, 200, { etag: `"${written.etag}"` });
}

export default {
  /**
   * @param {Request} request
   * @param {Env} env
   * @returns {Promise<Response>}
   */
  async fetch(request, env) {
    const url = new URL(request.url);

    // Liveness only, and deliberately says nothing else: an unauthenticated
    // caller should not be able to fingerprint which version you run.
    if (url.pathname === '/v1/health') return json({ ok: true });

    const expected = (env.SYNC_TOKEN ?? '').trim().toLowerCase();
    if (!expected) {
      // The one misconfiguration worth spelling out, because it happens after
      // a code-only redeploy and otherwise looks like a broken client.
      return err(
        503,
        'this worker has no SYNC_TOKEN secret — run `wrangler secret put SYNC_TOKEN`, ' +
          'or add it under Settings → Variables and Secrets in the dashboard',
      );
    }

    const token = bearer(request);
    if (!token) return err(401, 'missing or malformed bearer token');
    if (!constantTimeEqual(token, expected)) return err(401, 'token rejected');

    // Behind auth on purpose: it doubles as a credential check, and it cannot
    // be used to survey deployments from outside.
    if (url.pathname === '/v1/meta') {
      return json({ version: WORKER_VERSION, features: WORKER_FEATURES });
    }

    if (url.pathname !== '/v1/blob') return err(404, 'not found');

    const key = await storageKey(token);

    switch (request.method) {
      case 'GET':
        return handleGet(env, key);
      case 'PUT':
        return handlePut(request, env, key);
      case 'DELETE':
        await env.SYNC.delete(key);
        return json({ ok: true });
      default:
        return err(405, 'method not allowed');
    }
  },
};
