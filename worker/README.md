# The Litecter sync backend

Litecter's watch list lives in SQLite on your Mac. This Worker keeps an
encrypted copy of it somewhere that a lost disk can't take with it.

**You run it, not us.** There is no litecter.cc account, no server holding your
list, and no bill anyone else pays. You deploy this file to your own Cloudflare
account in about a minute, paste the URL into Litecter, and that's the feature.

Everything here fits inside Cloudflare's free plan by a wide margin: a typical
document is under 100 KB against 10 GB of free storage, and about three syncs a
day against a million free operations a month.

## What it can and can't see

[`src/index.js`](src/index.js) is the entire backend — one file, no
dependencies, no build step. What you paste is what you're reading.

It stores exactly one object: your watch list, sealed on your machine with
XChaCha20-Poly1305 before it is ever sent. The key never leaves your Mac, so
this Worker holds bytes it cannot read. It sees an opaque blob and a bearer
token, and knows nothing about what a watch list even is.

The flip side is worth saying plainly: **lose your sync key and the backup is
gone.** Nothing in your Cloudflare account can decrypt it. Litecter shows you
the key once, at setup, and tells you to save it — that's the moment that
matters.

## Deploy it

Three routes, same result. Pick by what you already have installed.

### Browser only — no tools at all

The fastest route, and it needs nothing but a Cloudflare login.

1. **Workers & Pages → Create → Start with Hello World! → Deploy.** Name it
   `litecter-sync`.
2. **Edit code**, select everything, and paste in Litecter's *Copy worker code*
   button (Settings → Backup → Set up backup → Browser). Deploy.
3. **R2 → Create bucket**, named `litecter-sync`.
4. Back in the Worker: **Settings → Bindings → Add → R2 bucket.** Variable name
   `SYNC`, bucket `litecter-sync`.
5. **Settings → Variables and Secrets → Add → Secret.** Name `SYNC_TOKEN`,
   value the token Litecter shows you. Deploy.
6. Copy the Worker's `*.workers.dev` URL into Litecter and press **Connect**.

### An AI agent with shell access

Litecter generates the whole prompt for you — Settings → Backup → Set up
backup → Agent. It carries your token, so paste it into your own agent and
nowhere else. The app says so at the copy button.

### Terminal

```bash
git clone https://github.com/boat-builder/litecter.git
cd litecter/worker
cp wrangler.toml.example wrangler.toml
npx wrangler r2 bucket create litecter-sync
npx wrangler deploy
npx wrangler secret put SYNC_TOKEN     # paste the token Litecter shows you
```

`deploy` prints the URL. Paste it into Litecter.

Not cloning? The same file is attached to every release:

```bash
curl -fsSLO https://github.com/boat-builder/litecter/releases/latest/download/litecter-worker.js
```

## Keeping it current

Your deployment doesn't update itself, and it doesn't need to — Litecter keeps
working while it's behind. But new versions land, so the app checks: it ships a
copy of this file, reads `WORKER_VERSION` out of it, asks your deployment what
version it's running, and tells you when the two differ. Settings → Backup →
*Update backend worker…* has the same three routes.

**Redeploying code preserves everything else.** Your bucket binding, your
`SYNC_TOKEN`, your custom domain all survive a code-only update — so an update
is one paste, with no re-configuration and nothing to re-key. That's also why
the update instructions carry no secret and are safe to paste anywhere.

The one exception is a deployment made before versioning existed: it has no
`SYNC_TOKEN` at all, so you'll need to add it once (step 5 above). The app
detects this case and says so.

## The API

Every route except `/v1/health` needs `Authorization: Bearer <token>`, matched
against the `SYNC_TOKEN` secret. Without that secret set, everything returns
`503` with a message telling you so.

| | | |
|---|---|---|
| `GET` | `/v1/meta` | `{"version":1,"features":["blob","meta"]}` — also a credential check |
| `GET` | `/v1/blob` | The sealed bytes and an `ETag`. `404` if none yet. |
| `PUT` | `/v1/blob` | Replaces it. Send `If-Match` with the ETag from `GET`; `412` if another device wrote first. Omitting `If-Match` means "there was nothing when I pulled". |
| `DELETE` | `/v1/blob` | Removes it. This is what "erase my backup" does. |
| `GET` | `/v1/health` | `{"ok":true}`. Unauthenticated, and says nothing else — a stranger shouldn't be able to fingerprint your deployment. |

Bodies are capped at 8 MB; the client targets 4 MB and sheds snapshot text to
stay under it.

The object lives at `blobs/<sha256("litecter-sync-v1:" + token)>`. The path is
computed here rather than accepted from the client, so a token can only ever
address its own object, and your bucket listing never shows the token.

## Removing it

In order, because the middle step fails otherwise:

1. **Erase the data** — Litecter → Settings → Backup → *Delete cloud backup*.
   Do this from the app: R2 refuses to delete a bucket that still has objects
   in it, and neither the CLI nor the dashboard bulk-deletes them for you.
2. **Delete the Worker and the bucket** in the dashboard, or
   `npx wrangler delete` and `npx wrangler r2 bucket delete litecter-sync`.
3. **Disconnect** in Litecter, which forgets the endpoint and the key.

## Notes for whoever maintains this

- `WORKER_VERSION` lives in one place, and the app *parses* it out of the
  shipped copy rather than declaring its own number. Bump it in the same commit
  as any change to this file and the app knows automatically.
- Bump it even for changes with no API surface. The version is how a fix
  reaches deployments; a change that doesn't bump it never arrives.
- The API only grows. A field is never repurposed, because someone out there is
  running a version you shipped a year ago and it has to keep working.
- Don't add a build step or minify anything. People are being asked to
  trust-paste this into their own cloud account; it stays legible.

Design notes and the client side are in [../docs/sync.md](../docs/sync.md); the
deploy/update flow itself is
[../docs/self-hosted-backend.md](../docs/self-hosted-backend.md).
