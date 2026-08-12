# Litecter — Requirement Spec & Implementation Overview

A self-hosted page-change monitor. Watches a list of URLs on a per-URL schedule,
detects meaningful content changes, and nags you until you've reviewed them.

- **Now:** macOS desktop app. Launches at login, lives in the menu bar, simple UI.
- **Later:** headless CLI/daemon on Linux servers. Same engine, same database.

---

## 1. Product requirements

### 1.1 URL management
- Add a URL in one action: paste → Enter. Title and favicon are fetched automatically.
- Bulk add: paste multiple lines / import from a text file — one URL per line.
- Remove: single or bulk (multi-select). Removal asks for confirmation only for bulk.
- Edit per-URL settings: check frequency, optional CSS selector (watch part of a page),
  optional ignore patterns (regex lines to strip before diffing).

### 1.2 Scheduling
- Frequencies: **hourly / daily / weekly / monthly**. Default: **weekly**.
- Global default frequency in Settings; per-URL override.
- Checks are spread with jitter — 1,000 weekly URLs must not fire in the same minute.
- "Check now" available per URL and for the whole list.

### 1.3 Change detection
- **Every check renders the page in a real browser engine** (headless Chromium) —
  JavaScript executes, the DOM settles, and we compare what a human would actually
  see. SPAs and client-rendered pages work out of the box.
- Compare the **rendered visible text** (`innerText` of the page or a selector), not
  raw HTML — markup churn, rotating asset hashes, and CSRF tokens don't cause false
  positives, and hidden elements are excluded automatically.
- Optional per-URL CSS selector restricts comparison to one region of the page.
- Optional per-URL ignore-regex filters (e.g. strip a "last updated" timestamp line).
- Optional per-URL wait-for-selector / settle delay for slow-loading apps.
- Errors (404, timeout, DNS, render failure) are a visible URL state, never silent.

### 1.4 Review workflow — the inbox model
The core UX insight: **this is an inbox**. A change is "unread mail" until you mark it seen.

- A URL with an unreviewed change holds exactly **one** unseen item, no matter how many
  times it changed since you last looked. The diff shown is always
  **last-seen snapshot → latest snapshot** (cumulative). No inbox pile-up.
- Marking seen moves the "last seen" pointer to the latest snapshot.
- "Mark all seen" exists and is one click + confirm.

### 1.5 Notifications
- **Daily digest only:** once a day (default 09:00, configurable), if any unseen
  changes exist: "Litecter: N pages have unreviewed changes". Repeats every day
  until the inbox is clear. Clicking it opens the app on the Changes view.
- No per-change pings — the menu-bar badge updates live when a change is detected;
  the notification waits for the digest hour.

### 1.6 Scale & platform targets
- Design target: **5,000 URLs** without UI or scheduler degradation.
- macOS 13+ (Apple Silicon + Intel). Linux x86_64/aarch64 for the CLI later.
- App auto-starts at login; closing the window hides to the menu bar (app keeps running).
- Single instance enforced.

### 1.7 Non-goals (v1)
- Pages behind login (the browser engine makes this *possible* later via a persistent
  profile — flagged as a v2 candidate, not in v1).
- Screenshot/visual diffing (we render in a browser anyway, so this is a natural v2
  add-on; v1 diffs rendered text only).
- Bot-wall bypass: sites that hard-block automated browsers (some Cloudflare
  configurations) will surface as errors, not silently skip.
- Mobile, multi-user, cloud sync.

---

## 2. UI / UX design

### 2.1 Structure: two views + a persistent add bar

```
┌────────────────────────────────────────────────────────────────┐
│  [＋ Paste or type a URL…                      ]   ⚙ Settings  │
│   Changes (12)  │  Library (1,847)                    [ / 🔍]  │
├────────────────────────────────────────────────────────────────┤
│  CHANGES — unseen first, newest first                          │
│                                                                │
│ ● docs.stripe.com  Webhooks — API reference     2h ago  +41 −8 │
│    "…retry schedule changed from 3 days to 5 days…"            │
│ ● ferrous-systems.com  Training schedule        1d ago  +12 −2 │
│ ○ blog.rust-lang.org  (seen yesterday)                  +90 −4 │
│                                                                │
│  [Mark all seen]                                               │
└────────────────────────────────────────────────────────────────┘
```

**Changes view (default, the inbox).** Only URLs with changes, unseen (●) pinned above
recently-seen (○). Each row: favicon, page title, domain, when, +added/−removed line
counts, and a one-line snippet of the first changed line. Click → diff detail.

**Diff detail.** Unified text diff, green/red line highlighting, unchanged regions
collapsed with "show context" expanders. Header: title, URL (click opens browser),
changed-at, snapshot history dropdown (compare any two). Actions: **Mark seen**,
Check now, Edit settings. `←`/`Esc` returns to list.

**Library view (management).** A virtualized table of everything — renders 5k rows
smoothly because only visible rows mount.

```
│  🔍 filter…        [All ▾ status] [All ▾ schedule]  1,847 URLs │
│ ☐  Title / URL             Sched.   Last check   Next    State │
│ ☐  Stripe API docs         weekly   2h ago       in 6d   ● chg │
│ ☐  HN front page           hourly   12m ago      in 48m  ok    │
│ ☐  vendor-x.com/pricing    daily    1d ago       in 2h   ⚠ 404 │
│    └ 3 selected:  [Change schedule ▾] [Check now] [Remove]     │
```

Fuzzy search over title + URL (SQLite FTS5). Filters: state (changed / ok / error),
schedule. Sortable columns. Multi-select → bulk schedule change / check / remove.

### 2.2 Adding — the friction budget is zero
- The add bar is always visible in both views. Paste → Enter → done (default schedule,
  toast with "Undo"). No dialog for the common case.
- Paste with multiple lines → inline preview "Add 37 URLs on weekly schedule?" → Enter.
- `⌘N` focuses the add bar from anywhere.

### 2.3 Keyboard model (email muscle memory)
`j/k` move · `Enter` open diff · `e` mark seen (works in list and detail) ·
`/` search · `⌘N` add · `⇧E` mark all seen. Fully mouse-usable too.

### 2.4 Seen semantics
Opening a diff does **not** auto-mark it seen (a misclick shouldn't silence the daily
nag — reviewing is an explicit act). A Settings toggle enables auto-mark-on-open for
users who prefer email behavior.

### 2.5 Menu bar
Tray icon with unseen badge count. Menu: Open Litecter · N unseen changes ·
Check all now · Pause checks (1h / until resumed) · Launch at login ✓ · Quit.

### 2.6 States
- Empty: single centered add bar, "Paste your first URL".
- First snapshot of a URL = baseline; shows as "watching since …", never as a change.
- Error rows surface in Library with a ⚠ state + last error message; after 5
  consecutive failures a low-priority notification is sent once.

---

## 3. Architecture

### 3.1 Workspace layout — one engine, two shells

```
litecter/
├── crates/
│   ├── litecter-core/     # engine: store, scheduler, fetcher, differ, notify — no UI deps
│   └── litecter-cli/      # `litecter` binary: CLI + `litecter daemon` (servers, cron)
├── app/                   # Tauri 2 desktop shell (macOS now; Linux/Windows builds free later)
│   ├── src-tauri/         # thin Rust layer: embeds litecter-core, exposes IPC commands
│   └── src/               # frontend: Svelte + TypeScript, virtualized lists
└── SPEC.md
```

Everything lives in `litecter-core`; both shells are thin. The desktop app runs the
scheduler in-process (it's alive at login via the tray). On a server you run
`litecter daemon` under systemd, or `litecter check --due` from cron — same database,
so you can even move the DB file from Mac to server later.

### 3.2 Technology choices

| Concern        | Choice                          | Why |
|----------------|--------------------------------|-----|
| Desktop shell  | **Tauri 2** (stable, v2.10)    | Tray, autostart, notifications, single-instance as first-party plugins; ~5 MB app. A webview renders virtualized 5k-row lists, fuzzy search, and rich text diffs far better than egui/iced today, and the Rust engine stays 100% shared. |
| Frontend       | Svelte + TypeScript            | Small, fast, no runtime bloat; `virtua` for list virtualization. (Swappable — the frontend is dumb; all logic is Rust.) |
| **Page rendering** | **chromiumoxide** (Chrome DevTools Protocol) | Drives one shared headless Chromium from async Rust. Real rendering engine, real JS execution — identical behavior on macOS and Linux servers. Note: the Tauri webview is *not* used for checks; checks always go through headless Chromium so the desktop app and server daemon detect identically. |
| Browser binary | System Chrome/Chromium/Edge, else auto-download pinned Chromium | Zero-setup on a Mac with Chrome installed; `apt install chromium` or the built-in fetcher on servers. |
| Async runtime  | tokio                          | Standard; chromiumoxide is tokio-native. |
| HTTP (aux)     | reqwest (rustls)               | Favicons and other non-check fetches only. |
| Text extraction | CDP `Runtime.evaluate` → `innerText` | Rendered visible text of `document.body` or the per-URL selector; Rust side then applies ignore-regexes + whitespace normalization. |
| Diff           | similar                        | Battle-tested Myers/patience diff (by @mitsuhiko), produces unified diffs. |
| Store          | SQLite via rusqlite (WAL mode) | Single file, FTS5 search, trivially handles 5k URLs; zero ops. |
| Snapshots      | zstd-compressed text in SQLite | ~20 KB page text → ~4 KB. 5k URLs × 10 snapshots ≈ 200 MB worst case. |
| Hash           | blake3                         | Fast content fingerprint for the "did it change" fast path. |
| CLI            | clap (derive)                  | Standard. |
| Notifications  | tauri-plugin-notification (app) / notify-rust (CLI) | Native on both platforms. |

### 3.3 Data model (SQLite)

```sql
urls(id, url UNIQUE, title, favicon, schedule,          -- 'hourly'|'daily'|'weekly'|'monthly'
     selector, ignore_patterns,                         -- nullable per-URL options
     wait_selector, settle_ms,                          -- nullable render-wait overrides
     next_check_at, last_checked_at,
     status,                                            -- 'ok'|'error'|'paused'
     error_message, error_count,
     last_seen_snapshot_id,                             -- the inbox pointer (§1.4)
     created_at)
snapshots(id, url_id, fetched_at, content_hash, text_zstd BLOB)
changes(id, url_id, detected_at, seen_at,               -- seen_at NULL = unseen
        from_snapshot_id, to_snapshot_id,               -- to_ advances on re-change while unseen
        lines_added, lines_removed, first_change_snippet)
settings(key, value)                                    -- default schedule, nag hour, retention…
urls_fts(FTS5: title, url)                              -- instant search at 5k rows
```

Retention: keep the last N snapshots per URL (default 10) **plus** any snapshot
referenced by a change or seen-pointer. Nightly vacuum job.

### 3.4 Browser engine management

Litecter is **asleep by default** — no browser process exists at rest:

- **Wake → check → sleep:** when the scheduler tick finds due URLs, it launches
  headless Chromium, drains the queue, and kills the process (30 s idle grace).
  Between triggers, Litecter's footprint is one idle tokio timer and a SQLite file.
- **Single tab:** checks run serially in one reused tab, blanked (`about:blank`)
  between checks. `concurrency` is a config knob (default **1**) if scale ever
  demands more.
- **Recycling:** within an unusually long batch, the browser process is restarted
  after ~300 page loads — caps the slow memory creep every long-lived Chromium has.
- **Crash supervision:** if Chromium dies mid-batch, in-flight checks are retried
  once on a fresh process; a crash is never recorded as a "page changed" event.
- **Resource blocking:** CDP network interception blocks images, media, fonts, and
  known analytics beacons. JS, CSS, and XHR/fetch still run (they shape the DOM).
  Cuts bandwidth and settle time dramatically at 1000s-of-URLs scale.
- **Wait strategy:** navigation → `load` event → network-quiet for 500 ms, hard cap
  15 s — then extract. Per-URL `wait_selector` / `settle_ms` override for stubborn SPAs.
- Headless "new" mode with a normal desktop user-agent string.

### 3.5 Scheduler — one loop, not 5,000 timers

A single tick every 60 s runs `SELECT … WHERE next_check_at <= now AND status != 'paused'`
and feeds due URLs into the single-tab check queue:

- Serial checks (concurrency = 1, see §3.4).
- **Minimum 10 s gap per host** — 200 URLs on one domain check politely spread out,
  never as a burst.
- On completion: `next_check_at = now + interval ± 5% jitter` (spreads load drift).
- Back-pressure safe: a slow batch delays checks, never overlaps them.
- Throughput: one tab ≈ **12–15 rendered pages/min**. 5,000 weekly URLs need
  ~0.5/min; 5,000 daily ~3.5/min — comfortably serial. Only thousands-on-*hourly*
  would ever need the `concurrency` knob raised.

### 3.6 Check pipeline (per URL)

```
acquire tab from pool
  → navigate (redirects followed by the browser; load + network-quiet, cap 15 s)
  → [wait_selector / settle_ms if configured]
  → evaluate: innerText of (selector ?? document.body)
  → apply ignore-regexes → normalize whitespace → blake3 hash
  → hash == latest snapshot? → done
  → store new snapshot
  → first snapshot ever? → baseline, done (no change event)
  → upsert change row (extend `to_snapshot_id` if one is already unseen)
  → tray badge count updates; notification waits for the daily digest
```

Failures increment `error_count` (exponential backoff ×2 up to one interval max);
success resets it.

### 3.7 Notifications & re-nag
- Digest-only: no immediate per-change notifications; the tray badge updates live.
- Daily digest job at the configured hour: `count(changes WHERE seen_at IS NULL)` > 0 →
  one summary notification. Repeats every day until the inbox is clear. Runs inside
  the same scheduler loop.

### 3.8 CLI surface (`litecter-cli`, works today on macOS, later on servers)

```
litecter add <url> [--every weekly] [--selector "main"]
litecter add --from-file urls.txt
litecter list [--filter changed|error] [--json]
litecter rm <id|url>…
litecter check [--all | --due | <id|url>]
litecter changes [--unseen] [--json]
litecter diff <id> [--from <snap> --to <snap>]
litecter seen <id|--all>
litecter daemon                    # long-running scheduler (systemd unit provided)
litecter export / import           # JSON dump for backup / mac→server migration
```

DB at `~/Library/Application Support/litecter/litecter.db` (macOS) /
`$XDG_DATA_HOME/litecter/` (Linux). `--json` on read commands for scripting.
Server notification channels (webhook/email/ntfy) are a v2 item — the notifier is a
trait with desktop as the first impl.

Server prerequisite: a Chromium binary (`apt install chromium-browser` or
`litecter browser install` to auto-download the pinned build). Headless Chromium runs
fine on servers without a display.

### 3.9 macOS integration
- **Launch at login:** tauri-plugin-autostart (toggle in Settings + tray menu, default on).
- **Close-to-tray:** window close hides; app quits only via tray → Quit.
- **Single instance:** tauri-plugin-single-instance; second launch focuses the window.
- Distribution: local `cargo tauri build` — signing/notarization unnecessary for
  your own machine.

---

## 4. Milestones

| # | Deliverable | Proves |
|---|-------------|--------|
| **M1** | `litecter-core` + CLI: add/list/rm, browser-rendered `check`, snapshots, diffing, SQLite, `changes`/`seen` | Detection quality end-to-end (incl. SPAs), testable in a terminal |
| **M2** | Scheduler + daemon mode + macOS notifications from CLI daemon | The nag loop, politeness, backoff |
| **M3** | Tauri app: Changes inbox + diff view + add bar + Library, tray badge | The full daily-driver UX |
| **M4** | Autostart, daily re-nag, bulk ops, keyboard shortcuts, error surfacing, import/export | Polish → runs at login, disappears until needed |

M1+M2 make it useful (cron + terminal) before any UI exists; M3/M4 make it pleasant.

---

## 5. Decisions (resolved 2026-08-12)

1. **Frontend stack** — Svelte + TypeScript inside Tauri (user had no preference).
2. **Notifications** — daily digest only; no per-change pings.
3. **Retention** — last 10 snapshots per URL.
4. **Logged-in pages** — out of scope for now.
5. **Browser lifecycle** — single tab, browser launched per due-batch and torn down
   after; fully asleep between triggers (user-confirmed).
