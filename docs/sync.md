# Cloud sync

Litecter's database lives on one machine. A lost disk used to mean a lost watch
list. Sync keeps an encrypted copy in Cloudflare R2 so a new machine can pick up
where the old one left off.

**The R2 bucket is the user's own.** Litecter runs no sync service: each user
deploys a small Worker to their own Cloudflare account, on the free plan, and
that is the backend. This document is about the data — what travels, and how two
machines reconcile. The deploy, update and teardown flow is
[self-hosted-backend.md](self-hosted-backend.md).

The app stays local-first: SQLite remains the only thing the UI reads, so the
window is instant and works offline. Sync is a background errand, never a
dependency.

## What is actually backed up

The database is roughly 99% snapshots by size and 1% irreplaceable state by
value. Snapshots are page text that a single check regenerates; the watch list is
hand-curated and gone forever with the disk. So the document carries:

- the watch list — URL, schedule, selector, ignore patterns, wait selector, settle time
- `digest_hour` (the only synced setting; everything else is machine-local)
- tombstones, so a delete on one machine isn't resurrected by another
- **unreviewed changes, with the two snapshot texts their diff needs**

That last one is the deliberate exception. Read history is disposable; the inbox
you haven't worked through is not. Everything else — seen changes, snapshot
history, error counts, `next_check_at` — is rebuilt locally.

A typical document is well under 100 KB. Snapshot texts are the only part that
can grow, and they are shed biggest-first if the document would exceed 4 MB
(the endpoint's own ceiling is 8 MB). A pending change that loses its text keeps
its counts and simply re-baselines on the next check.

## Identity: the sync key

There are no accounts. At setup a device generates 32 random bytes and that
secret *is* the connection. Two independent values come out of it:

```
root ──derive("litecter sync auth v1")───────► auth token — the backend's SYNC_TOKEN
     ──derive("litecter sync encryption v1")─► cipher key — never leaves the device
```

Deriving rather than generating separately is what keeps restoring simple: the
same key that decrypts the backup also authenticates to it, so a second machine
needs one secret, not two. The token is what the user pastes into their Worker
as `SYNC_TOKEN`; the cipher key has no counterpart anywhere else.

The storage path is **not** derived on the client: the Worker hashes the bearer
token it receives, so the token never appears in a bucket listing and a client
can only ever address its own object. The body is sealed with
XChaCha20-Poly1305 over zstd-compressed JSON, so the backend stores bytes it
cannot read — which matters even though the bucket belongs to the user, because
it means a leaked Cloudflare session does not leak a watch list.

The trade is explicit and worth stating to users plainly: **lose the key, lose
the backup.** Nothing on the server can recover it. In exchange Litecter holds
no email addresses, no passwords and no readable watch lists, and needed no
account system to ship.

A connection is now two values — the key and the address of the user's
backend — so they travel together as one paste:

```
A1B2-C3D4-…-Z9Y8@https://litecter-sync.alice.workers.dev
```

Not encoded, deliberately. A base64 blob would be shorter and totally opaque;
this is a string someone can look at and see what they are about to hand over.

The key is stored in the local `settings` table, which means it is only as
protected as the database file itself. That is the right level for now (the key
protects the *cloud* copy, not the local one), but moving it to the macOS
Keychain is the obvious hardening step.

## Merging, not overwriting

Two machines both check and both review, so "upload the newer file" would
silently destroy work. Merge is per URL, keyed on the URL string — already
`UNIQUE` in the schema, so no synthetic id was needed.

| field | rule |
|---|---|
| config (schedule, selector, filters) | last write wins on `updated_at` |
| `reviewed_at` | max — reviewing is monotonic |
| `pending` | taken wholesale from whichever side has the newer `pending_at` |
| deletion | tombstone wins if `deleted_at` is newer than `updated_at` |

`updated_at` moves only when a watch's *configuration* changes, never on a check
— otherwise every tick would look like something worth uploading.

Resolving `pending` by **who checked last** rather than "who has one" is the
subtle part, and it is what makes a revert propagate. When a page returns to its
reviewed state, `checker` drops the pending change. That machine now has no
pending and the newest `pending_at`, so its *absence* wins over the other
machine's stale change. Picking "whoever has a pending" would resurrect it
forever.

Merge is pure and order-independent: `merge(a, b)` and `merge(b, a)` agree except
on exact timestamp ties, which resolve to local so a machine never appears to
lose its own edit.

## The round

```
┌── pull ──► sealed bytes ──► open ──► remote doc ─┐
│                                                   ├─► merge ─┬─► apply to SQLite
└── build from SQLite ──────────────► local doc ───┘           └─► seal ─► push (If-Match)
```

Concurrency is optimistic. `GET` returns an ETag; `PUT` sends it back as
`If-Match`. A racing device gets 412, and the whole round runs again against the
document that won. Each attempt starts from strictly newer state, so this
converges rather than spins; it gives up after three.

`SyncSession` splits the phases so no database lock is ever held across a network
call. The desktop app shares its `Store` behind a mutex with the UI thread and
the checker — a sync holding that lock for an HTTP round-trip would freeze the
window for up to the 30-second timeout. `sync::drive` sequences the network and
defers every database touch to a closure the caller supplies.

## When it runs

- **Daily**, as the backup guarantee.
- **60 seconds after the watch list last changed**, debounced. Adding thirty URLs
  and losing the disk an hour later shouldn't cost thirty URLs, and the debounce
  collapses a bulk import into one upload.

Marking things seen does *not* trigger a push; it rides the daily. Review state
is cheap to lose and expensive to chase.

## When it fails

A backup you believe is running but isn't is worse than no backup, so a failure
is persisted (`sync_failing_since`, `sync_last_error`) rather than kept in memory
— it has to outlive the process that noticed it. `failing_since` dates the *first*
failure in a run, so what you see is how long the backup has actually been broken,
not how long since the last retry. A success clears all of it.

Three surfaces, because the app is one a user can go a week without opening:

- **A banner in the main window**, not dismissible, with a Retry button. It stays
  until a sync actually succeeds.
- **Settings**, with the error text and when the outage began.
- **One notification**, at most once per outage and only after a full day of
  failure. This is the second notification Litecter is allowed to send; see the
  note in `litecter_core::scheduler::tick` before adding a third.

`litecter sync status` reports the same state, so a headless daemon isn't silent
about it either.

Retries back off — 5 min doubling to an hour — so a laptop that is simply offline
doesn't hammer the network all day. A manual sync ignores the backoff: the user
asking is better evidence the network is back than any timer.

Each user's bucket holds one object of ~100 KB against 10 GB free, and sees
roughly three writes a day against a million free operations a month. The free
plan is not a tier this outgrows; it is three orders of magnitude of headroom.

## Restoring on a new machine

```bash
litecter sync link --set '<key>@<backend url>'
litecter sync
```

`litecter sync link` on the first machine prints that string; Settings → Backup
→ *Show connection for another machine* is the same thing in the app.

The watch list, settings and inbox arrive; snapshots do not. Restored URLs become
due immediately so the machine builds baselines rather than sitting idle.

One trap worth knowing about if you touch this code: `checker::persist_ok`
dereferences `urls.last_seen_snapshot_id` directly. A restored row pointing at a
snapshot id that no longer exists turns **every** check into a recorded error, so
`apply` clears that pointer for newly inserted rows. `store::clear_last_seen`
exists for exactly this, and
`a_restored_url_without_snapshots_baselines_instead_of_erroring` guards it.

## Without the cloud

`litecter export --out list.json` and `litecter import list.json` use the same
document and the same merge rules, so a file moved by hand carries exactly what
the cloud path carries.

## The backend

[`worker/src/index.js`](../worker/src/index.js) is one file with no dependencies
and no build step, deployed by the user to their own Cloudflare account. It is
deliberately dumb: bearer auth against a `SYNC_TOKEN` secret, size limits,
conditional writes, a version route, and nothing else. It has no idea what a
watch list is.

Requiring that secret is what closed the one real gap in the earlier design,
where any well-formed token could store 8 MB under its own hash. On a
single-tenant deployment the token is simply *the* token, so an unknown caller
gets a 401 and rate limiting stops being load-bearing.

Setup, updates and teardown are [their own document](self-hosted-backend.md) —
including why the app parses `WORKER_VERSION` out of the file it ships rather
than declaring a number of its own.
