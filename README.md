# Litecter

Watch web pages for changes — rendered in a real browser, reviewed like an inbox.

Every check loads the page in **headless Chromium** (JavaScript runs, so SPAs work) and
diffs the *rendered visible text* against what you last reviewed. Changes collect in an
inbox and a once-a-day notification nags you until you've marked them seen.

Ships as a macOS menu-bar app and a CLI/daemon for servers. Both share one Rust engine
and one SQLite file. [BACKLOG.md](BACKLOG.md) lists what's designed but not yet built.

## Layout

```
crates/litecter-core/   engine — store, browser renderer, differ, scheduler (no UI deps)
crates/litecter-cli/    `litecter` binary — CLI + `litecter daemon`
app/                    Tauri 2 + Svelte desktop app
  src/                  frontend (Changes inbox, Library, diff panel)
  src-tauri/            thin Rust shell — IPC commands, tray, autostart
BACKLOG.md              designed but not built — start here for something to pick up
```

All logic lives in `litecter-core`; both shells are thin. Adding a feature usually means
a core function plus a CLI subcommand and/or a `#[tauri::command]`.

## Build & run

Prereqs: Rust 1.90+, Node 20+, and a Chromium-family browser installed.

```bash
cargo build --release              # engine + CLI → target/release/litecter
cargo test                         # core unit tests
```

```bash
cd app && npm install && npx tauri dev    # desktop app, hot reload
cd app && npx tauri build                 # → target/release/bundle/macos/Litecter.app
```

## CLI

```bash
litecter add example.com docs.stripe.com/api      # weekly by default
litecter add --every daily --from-file urls.txt   # bulk import
litecter list                                     # everything watched
litecter check                                    # run due checks (--all to force)
litecter changes                                  # the inbox (* = unreviewed)
litecter diff 3                                   # diff since you last reviewed
litecter seen 3                                   # or: litecter seen --all
litecter daemon --digest-hour 9                   # scheduler loop + daily digest
```

`--json` on `list` and `changes` for scripting.

## How it works

- **Wake → check → sleep.** A 60 s tick queries SQLite for URLs whose `next_check_at`
  has passed. Only then does Chromium launch; it's killed once the batch drains. At rest
  there is no browser process. Checks are serial in a single tab, with a 10 s minimum gap
  per host and ±5% schedule jitter so large sets don't bunch up.
- **Comparison** is on `innerText` of the page (or a per-URL CSS selector), normalized and
  hashed with blake3 — markup churn and rotating tokens don't register as changes.
- **One unseen change per URL.** If a page changes again before you review it, the existing
  inbox item is *extended* rather than duplicated: the diff is always last-reviewed → latest.
  Marking seen advances `urls.last_seen_snapshot_id`.
- **Storage.** SQLite (WAL) with zstd-compressed text snapshots, last 10 kept per URL plus
  anything a change row still references.

Database: `~/Library/Application Support/litecter/litecter.db` (macOS) or
`$XDG_DATA_HOME/litecter/litecter.db` (Linux). Override with `LITECTER_DB` — handy for
running against a scratch DB during development.

Browser: auto-detected from the usual install paths; pin one with
`LITECTER_CHROME=/path/to/binary`.

## Desktop app notes

Launch at login is enabled on first run (it registers a LaunchAgent pointing at the
binary's current path, so install to `/Applications` before enabling — or re-toggle it in
Settings after moving the app). Closing the window hides to the menu bar; the tray icon
carries the unreviewed count and its menu holds Open / Add link / Check due now / Quit.

## Server deployment

```bash
sudo apt install chromium-browser        # or set LITECTER_CHROME
litecter add --from-file urls.txt
litecter daemon
```

```ini
[Unit]
Description=Litecter page-change monitor
After=network-online.target

[Service]
ExecStart=/usr/local/bin/litecter daemon
Restart=on-failure

[Install]
WantedBy=default.target
```

## Not built yet

Resource blocking during checks, list virtualization, FTS5 search, `j/k` navigation,
snapshot-history compare, `export`/`import`, pause, and UI editing of per-URL ignore
filters — see [BACKLOG.md](BACKLOG.md), which has the reasoning and an approach for each.
