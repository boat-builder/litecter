# Backlog

Designed but not built. Each item says what's missing, where it goes, why it matters,
and how to approach it. Nothing here is required for current use — the app works
without all of it.

---

## Scale & performance

### 1. Block images, media, and fonts during checks
**Where:** `crates/litecter-core/src/renderer.rs` — `Renderer::launch` / `render`.

Every check currently downloads the full page including images, video, web fonts, and
analytics beacons, none of which affect `innerText`. On an image-heavy page this is the
dominant cost in both bandwidth and settle time.

Use CDP network interception to block by resource type: `Network.setBlockedURLs`, or
`Fetch.enable` with request-stage patterns and fail non-document/script/stylesheet/xhr
requests. **Keep JS, CSS, and XHR/fetch** — they shape the DOM and blocking them changes
what gets extracted. chromiumoxide exposes these via
`chromiumoxide::cdp::browser_protocol::network` / `::fetch`.

Verify by checking that a JS-rendered page still diffs correctly afterwards (see the
`file://` fixture approach used during initial testing).

### 2. Virtualize the Library table
**Where:** `app/src/Library.svelte` — the `{#each filtered as u}` loop.

Every filtered row is mounted in the DOM. At a few thousand URLs, first render and
scrolling will bog down noticeably.

Mount only the visible window. `virtua` (Svelte-compatible) was the intended library, or
hand-roll it: fixed row height, `transform: translateY` on a spacer, slice `filtered` by
scroll offset. Rows are already uniform height, which makes this straightforward. The
`Changes` list needs the same treatment only if a user accumulates thousands of
*unreviewed* changes, which is unlikely given the one-item-per-URL invariant.

### 3. SQLite FTS5 search instead of a JS substring filter
**Where:** `crates/litecter-core/src/store.rs` (schema + a new search method),
`app/src/Library.svelte` (the `filtered` derivation).

Search currently loads every URL into the frontend and filters with
`String.includes`. It's fine at hundreds of URLs and wasteful at thousands.

Add an FTS5 virtual table over `title` and `url`, kept in sync with triggers on
`urls` (insert/update/delete), plus a `Store::search_urls(query)` method and a
`search_urls` Tauri command. This pairs naturally with item 2 — once the frontend no
longer holds every row, it can't filter client-side anyway.

### 4. Recycle the browser process inside long batches
**Where:** `crates/litecter-core/src/renderer.rs`.

The browser is launched per due-batch and killed after, so ordinary use never
accumulates memory. But a single batch of thousands of URLs is one long-lived Chromium,
and Chromium's memory creeps under sustained page loads.

Count page loads in `Renderer` and, past a threshold (~300), tear down and relaunch
transparently inside `render`. Callers shouldn't need to know.

---

## Check fidelity

### 5. Network-quiet wait strategy
**Where:** `crates/litecter-core/src/renderer.rs` — `render`, `DEFAULT_SETTLE_MS`.

The current wait is a fixed 1.5 s sleep after the navigation response. That's too long
for static pages and too short for slow SPAs.

Intended behavior: navigation → `load` event → wait for no in-flight network requests
for 500 ms → extract, with a hard 15 s cap. Track in-flight requests by subscribing to
`Network.requestWillBeSent` / `loadingFinished` / `loadingFailed` events. Keep the
per-URL `settle_ms` override as an escape hatch for pages this heuristic gets wrong.

### 6. Expose per-URL tuning that the engine already supports
**Where:** schema columns exist and are read; nothing writes them.

`urls.ignore_patterns`, `urls.wait_selector`, and `urls.settle_ms` are all read by
`Store` and honored by `Renderer`/`checker` — but only `selector` can be set (via
`litecter add --selector`). The rest are permanently NULL.

- `ignore_patterns` is a JSON array of regexes; matching lines are dropped before
  diffing (`textproc::compile_ignores` / `normalize`). This is the fix for a page with a
  "last updated: <timestamp>" line that changes on every check — currently there is no
  way to silence one without dropping the URL.
- `wait_selector` waits for a CSS selector to appear before extracting; `settle_ms`
  overrides the settle delay.

Needs CLI flags (`--ignore`, `--wait-for`, `--settle-ms`) on `add`, an `edit`
subcommand, and a per-URL settings panel in the app. An invalid regex is already
tolerated (silently skipped) so a bad pattern can't break checking — but the UI should
validate and say so rather than letting it fail quietly.

---

## Review UX

### 7. Keyboard navigation in the lists
**Where:** `app/src/App.svelte` (`onKeydown`), `app/src/ChangesList.svelte`.

Implemented: `⌘N` focus add bar, `/` jump to Library search, `e` mark seen in the diff
panel, `Esc` close.

Missing: `j`/`k` to move a selection cursor through the Changes list, `Enter` to open the
highlighted row, `e` to mark seen from the list (not just the diff), `⇧E` mark all seen.
Needs a selected-index in `ChangesList` with a visible focus ring and scroll-into-view.

### 8. Snapshot history and collapsed context in the diff
**Where:** `app/src/DiffPanel.svelte`, `get_diff` in `app/src-tauri/src/main.rs`.

The diff panel shows exactly one comparison: last-reviewed → latest. Up to 10 snapshots
per URL are retained, so arbitrary pairs could be compared.

Add a snapshot dropdown (needs a `list_snapshots(url_id)` store method and command, and
a `get_diff_between(from_id, to_id)`), and collapse long unchanged regions behind a
"show context" expander — `differ::unified` takes a context radius, or switch the panel
to iterating `similar`'s grouped ops for real fold/unfold.

### 9. Auto-mark-seen-on-open toggle
**Where:** `app/src/Prefs.svelte`, `set_prefs`/`get_prefs` in `app/src-tauri/src/main.rs`.

Opening a diff deliberately does not mark it seen — reviewing is an explicit act, so a
misclick can't silence the daily digest. Some people will want email-style behavior
instead. Add a settings toggle (persist in the `settings` table alongside
`digest_hour`) that marks seen on open when enabled.

### 10. Favicons in the lists
**Where:** `crates/litecter-core/src/store.rs` (schema), `checker.rs`,
`app/src/ChangesList.svelte` + `Library.svelte`.

Rows show title and domain only. A favicon makes a long list scannable.

Add a `favicon` column, fetch once when a URL is first checked (the page is already open
in a browser — read `link[rel*=icon]` via `Runtime.evaluate`, or fall back to
`/favicon.ico`), store as a data URI or blob, render at 16px. Note `litecter-core` has
no HTTP client dependency right now — either add `reqwest` or fetch the bytes through
CDP to avoid it.

---

## Operational

### 11. ~~`litecter export` / `litecter import`~~ — done
Built alongside cloud sync: both reuse `sync::SyncDoc`, so a hand-moved file carries
exactly what the cloud path carries and merges by the same rules. See
[docs/sync.md](docs/sync.md).

Remaining nearby work: the sync key lives in the `settings` table, so it is only as
protected as the database file. Moving it to the macOS Keychain is the obvious hardening
step.

The open-storage gap is closed — the backend now requires a `SYNC_TOKEN` secret that only
the user's own key derives to, so an unknown caller gets a 401 rather than 8 MB of storage.
Each deployment is single-tenant, which is what made that possible; see
[docs/self-hosted-backend.md](docs/self-hosted-backend.md).

### 12. `litecter browser install`
**Where:** `crates/litecter-core/src/renderer.rs` — `find_browser`.

`find_browser` checks known install paths and `LITECTER_CHROME`, then fails with
"is it installed?". On a bare server that means the operator has to install Chromium
themselves.

Add a subcommand that downloads a pinned Chromium build to the data dir and a lookup
fallback to that path. chromiumoxide has a `fetcher` feature for exactly this (currently
disabled).

### 13. Pause checks
**Where:** `urls.status` accepts `'paused'` and `Store::due_urls` already excludes it —
but nothing ever sets it.

Wire it up: a tray menu item ("Pause checks" for 1h / until resumed), a
`litecter pause`/`resume` command, and per-URL pause in the Library. Global pause needs
a settings flag rather than mutating every row, so `due_urls` (or its caller) should
check that flag too.

### 14. Launch-at-login toggle in the tray menu
**Where:** `app/src-tauri/src/main.rs` — the `TrayIconBuilder` menu.

The tray menu has Open / Add link / Check due now / Quit. Autostart is only reachable through
Settings. Add it as a checkable tray item (`CheckMenuItem`) reading
`app.autolaunch().is_enabled()`.

Related gotcha worth preserving: the autostart plugin registers a LaunchAgent pointing
at the binary's *current* path, so enabling it from a build directory and then moving
the app leaves a dead agent. Re-toggling after install fixes it.

### 15. Notify on repeated check failures
**Where:** `crates/litecter-core/src/scheduler.rs` (`tick`), and the app's
`scheduler_loop` in `app/src-tauri/src/main.rs`.

Failures are recorded on the row with exponential backoff and shown with a `⚠` in the
Library, but nothing tells you a URL has gone permanently bad — a 404'd page just
silently stops producing changes.

Send one low-priority notification after ~5 consecutive failures, and don't repeat it
until the URL succeeds again (`error_count` is already tracked and reset on success).

### 16. Vacuum after pruning
**Where:** `crates/litecter-core/src/store.rs` — `prune_snapshots`.

Pruning deletes snapshot rows but SQLite doesn't return the pages to the filesystem, so
the DB only grows. Add a periodic `VACUUM` (nightly, or after N prunes) from the
scheduler loop. Cheap to do, easy to forget.
