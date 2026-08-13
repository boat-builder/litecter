use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

use crate::schedule::Schedule;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS urls (
    id INTEGER PRIMARY KEY,
    url TEXT NOT NULL UNIQUE,
    title TEXT,
    schedule TEXT NOT NULL DEFAULT 'weekly',
    selector TEXT,
    ignore_patterns TEXT,
    wait_selector TEXT,
    settle_ms INTEGER,
    next_check_at INTEGER NOT NULL,
    last_checked_at INTEGER,
    status TEXT NOT NULL DEFAULT 'ok',
    error_message TEXT,
    error_count INTEGER NOT NULL DEFAULT 0,
    last_seen_snapshot_id INTEGER,
    created_at INTEGER NOT NULL,
    -- Sync bookkeeping. `updated_at` moves only when the *config* of a watch
    -- changes (schedule, selector, filters) — never on a check, or every tick
    -- would look like something worth uploading. `reviewed_at` is the last time
    -- the user marked this URL seen; it's what lets a merge decide whether a
    -- remote pending change has already been reviewed elsewhere.
    updated_at INTEGER NOT NULL DEFAULT 0,
    reviewed_at INTEGER NOT NULL DEFAULT 0
);

-- `due_urls` runs on every scheduler tick forever, and it is the only query on
-- this table that can't be answered from the primary key.
CREATE INDEX IF NOT EXISTS idx_urls_next_check ON urls(next_check_at);

CREATE TABLE IF NOT EXISTS snapshots (
    id INTEGER PRIMARY KEY,
    url_id INTEGER NOT NULL REFERENCES urls(id) ON DELETE CASCADE,
    fetched_at INTEGER NOT NULL,
    content_hash TEXT NOT NULL,
    text_zstd BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_snapshots_url ON snapshots(url_id, id);

CREATE TABLE IF NOT EXISTS changes (
    id INTEGER PRIMARY KEY,
    url_id INTEGER NOT NULL REFERENCES urls(id) ON DELETE CASCADE,
    detected_at INTEGER NOT NULL,
    seen_at INTEGER,
    from_snapshot_id INTEGER NOT NULL,
    to_snapshot_id INTEGER NOT NULL,
    lines_added INTEGER NOT NULL DEFAULT 0,
    lines_removed INTEGER NOT NULL DEFAULT 0,
    first_change_snippet TEXT
);
CREATE INDEX IF NOT EXISTS idx_changes_url ON changes(url_id);
-- Partial, because every hot path here asks the same question: which changes are
-- still unseen? `count_unseen` runs on every tick (tray badge + digest), and
-- `record_change` probes for the URL's open change on every detected change.
-- Indexing only the unseen rows keeps this tiny — it holds inbox depth, not history.
CREATE INDEX IF NOT EXISTS idx_changes_unseen ON changes(url_id) WHERE seen_at IS NULL;

CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT 0
);

-- A removed URL has to leave a trace, or the next sync from a machine that
-- still has it would resurrect it. Tombstones are pruned once they are older
-- than any plausible offline window (see Store::prune_tombstones).
CREATE TABLE IF NOT EXISTS tombstones (
    url TEXT PRIMARY KEY,
    deleted_at INTEGER NOT NULL
);
";

const URL_COLS: &str = "id, url, title, schedule, selector, ignore_patterns, wait_selector, settle_ms, \
     next_check_at, last_checked_at, status, error_message, error_count, last_seen_snapshot_id, created_at, \
     updated_at, reviewed_at";

#[derive(Debug, Clone, Serialize)]
pub struct UrlRow {
    pub id: i64,
    pub url: String,
    pub title: Option<String>,
    pub schedule: Schedule,
    pub selector: Option<String>,
    pub ignore_patterns: Vec<String>,
    pub wait_selector: Option<String>,
    pub settle_ms: Option<u64>,
    pub next_check_at: i64,
    pub last_checked_at: Option<i64>,
    pub status: String,
    pub error_message: Option<String>,
    pub error_count: i64,
    pub last_seen_snapshot_id: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub reviewed_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangeItem {
    pub id: i64,
    pub url_id: i64,
    pub url: String,
    pub title: Option<String>,
    pub detected_at: i64,
    pub seen_at: Option<i64>,
    pub from_snapshot_id: i64,
    pub to_snapshot_id: i64,
    pub lines_added: i64,
    pub lines_removed: i64,
    pub snippet: Option<String>,
}

/// The durable half of a watch — what a sync document carries. Grouped into a
/// struct so `upsert_url_config` stays one argument wide.
pub struct UrlConfig<'a> {
    pub url: &'a str,
    pub title: Option<&'a str>,
    pub schedule: Schedule,
    pub selector: Option<&'a str>,
    pub ignore_patterns: &'a [String],
    pub wait_selector: Option<&'a str>,
    pub settle_ms: Option<u64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub reviewed_at: i64,
}

pub struct Store {
    conn: Connection,
}

fn url_from_row(row: &Row<'_>) -> rusqlite::Result<UrlRow> {
    let schedule: String = row.get(3)?;
    let ignore_json: Option<String> = row.get(5)?;
    Ok(UrlRow {
        id: row.get(0)?,
        url: row.get(1)?,
        title: row.get(2)?,
        schedule: schedule.parse().unwrap_or(Schedule::Weekly),
        selector: row.get(4)?,
        ignore_patterns: ignore_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        wait_selector: row.get(6)?,
        settle_ms: row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
        next_check_at: row.get(8)?,
        last_checked_at: row.get(9)?,
        status: row.get(10)?,
        error_message: row.get(11)?,
        error_count: row.get(12)?,
        last_seen_snapshot_id: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
        reviewed_at: row.get(16)?,
    })
}

fn change_from_row(row: &Row<'_>) -> rusqlite::Result<ChangeItem> {
    Ok(ChangeItem {
        id: row.get(0)?,
        url_id: row.get(1)?,
        url: row.get(2)?,
        title: row.get(3)?,
        detected_at: row.get(4)?,
        seen_at: row.get(5)?,
        from_snapshot_id: row.get(6)?,
        to_snapshot_id: row.get(7)?,
        lines_added: row.get(8)?,
        lines_removed: row.get(9)?,
        snippet: row.get(10)?,
    })
}

/// `CREATE TABLE IF NOT EXISTS` is a no-op on a database that already exists,
/// so columns added after v0.1 have to be bolted on here. Adding a column is
/// idempotent-by-inspection rather than by version number: cheap, and it can't
/// get out of step with `SCHEMA` the way a migration counter can.
fn migrate(conn: &Connection) -> Result<()> {
    let columns = |table: &str| -> Result<HashSet<String>> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    };

    let url_columns = columns("urls")?;
    for (name, ddl) in [
        ("updated_at", "ALTER TABLE urls ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0"),
        ("reviewed_at", "ALTER TABLE urls ADD COLUMN reviewed_at INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !url_columns.contains(name) {
            conn.execute(ddl, [])?;
        }
    }
    if !columns("settings")?.contains("updated_at") {
        conn.execute(
            "ALTER TABLE settings ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    // Pre-migration rows have updated_at = 0, which would lose every merge
    // against a synced peer. Seed from created_at so an existing watch list
    // arrives with a defensible timestamp instead of the epoch.
    conn.execute(
        "UPDATE urls SET updated_at = created_at WHERE updated_at = 0",
        [],
    )?;
    // Likewise seed reviewed_at from the review trail already in `changes`.
    conn.execute(
        "UPDATE urls SET reviewed_at = COALESCE(\
             (SELECT MAX(c.seen_at) FROM changes c WHERE c.url_id = urls.id), 0) \
         WHERE reviewed_at = 0",
        [],
    )?;

    // Backup used to point at an endpoint the app hard-coded. Now that every
    // user deploys their own backend there is no default left to fall back on,
    // so an install that was already syncing is pinned to the address it was
    // already using. Without this, updating Litecter would switch off a working
    // backup and say nothing.
    //
    // Nothing outside this migration should ever name that address.
    const LEGACY_SHARED_ENDPOINT: &str = "https://sync.litecter.cc";
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value, updated_at) \
         SELECT 'sync_endpoint', ?1, 0 \
         WHERE EXISTS (SELECT 1 FROM settings WHERE key = 'sync_key' AND value <> '') \
           AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'sync_endpoint' AND value <> '')",
        [LEGACY_SHARED_ENDPOINT],
    )?;
    Ok(())
}

const CHANGE_QUERY: &str = "SELECT c.id, c.url_id, u.url, u.title, c.detected_at, c.seen_at, \
     c.from_snapshot_id, c.to_snapshot_id, c.lines_added, c.lines_removed, c.first_change_snippet \
     FROM changes c JOIN urls u ON u.id = c.url_id";

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating data dir {}", dir.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Self { conn })
    }

    /// A throwaway store with the current schema. Tests use it to exercise the
    /// real SQL rather than a stand-in.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Self { conn })
    }

    // ---- urls -------------------------------------------------------------

    pub fn add_url(
        &self,
        url: &str,
        schedule: Schedule,
        selector: Option<&str>,
        now: i64,
    ) -> Result<UrlRow> {
        self.conn
            .execute(
                "INSERT INTO urls (url, schedule, selector, next_check_at, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?4, ?4)",
                params![url, schedule.to_string(), selector, now],
            )
            .with_context(|| format!("adding {url} (already watched?)"))?;
        // Re-adding something previously deleted must clear the tombstone, or
        // the next sync would read the delete as newer and drop it again.
        self.conn
            .execute("DELETE FROM tombstones WHERE url = ?1", [url])?;
        let id = self.conn.last_insert_rowid();
        Ok(self.url_by_id(id)?.expect("row just inserted"))
    }

    pub fn url_by_id(&self, id: i64) -> Result<Option<UrlRow>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {URL_COLS} FROM urls WHERE id = ?1"),
                [id],
                url_from_row,
            )
            .optional()?)
    }

    /// Resolve a CLI target: "#7"/"7" as an id, otherwise by URL
    /// (with an implied https:// if the scheme was omitted).
    pub fn resolve_url(&self, target: &str) -> Result<Option<UrlRow>> {
        if let Ok(id) = target.trim_start_matches('#').parse::<i64>()
            && let Some(u) = self.url_by_id(id)?
        {
            return Ok(Some(u));
        }
        let by_url = |url: &str| -> Result<Option<UrlRow>> {
            Ok(self
                .conn
                .query_row(
                    &format!("SELECT {URL_COLS} FROM urls WHERE url = ?1"),
                    [url],
                    url_from_row,
                )
                .optional()?)
        };
        let mut found = by_url(target)?;
        if found.is_none() && !target.contains("://") {
            found = by_url(&format!("https://{target}"))?;
        }
        Ok(found)
    }

    pub fn list_urls(&self) -> Result<Vec<UrlRow>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {URL_COLS} FROM urls ORDER BY id"))?;
        let rows = stmt.query_map([], url_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn due_urls(&self, now: i64) -> Result<Vec<UrlRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {URL_COLS} FROM urls \
             WHERE next_check_at <= ?1 AND status != 'paused' ORDER BY next_check_at"
        ))?;
        let rows = stmt.query_map([now], url_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Deleting records a tombstone so the removal survives a sync round-trip
    /// with a machine that still has the URL. `now` dates the tombstone.
    pub fn remove_url(&self, id: i64, now: i64) -> Result<()> {
        let url: Option<String> = self
            .conn
            .query_row("SELECT url FROM urls WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        self.conn.execute("DELETE FROM urls WHERE id = ?1", [id])?;
        if let Some(url) = url {
            self.conn.execute(
                "INSERT INTO tombstones (url, deleted_at) VALUES (?1, ?2) \
                 ON CONFLICT(url) DO UPDATE SET deleted_at = excluded.deleted_at",
                params![url, now],
            )?;
        }
        Ok(())
    }

    pub fn update_check_ok(
        &self,
        url_id: i64,
        title: Option<&str>,
        now: i64,
        next_check_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE urls SET last_checked_at = ?2, next_check_at = ?3, status = 'ok', \
             error_message = NULL, error_count = 0, title = COALESCE(?4, title) \
             WHERE id = ?1",
            params![url_id, now, next_check_at, title],
        )?;
        Ok(())
    }

    pub fn update_check_err(
        &self,
        url_id: i64,
        message: &str,
        now: i64,
        retry_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE urls SET last_checked_at = ?2, next_check_at = ?3, status = 'error', \
             error_message = ?4, error_count = error_count + 1 \
             WHERE id = ?1",
            params![url_id, now, retry_at, message],
        )?;
        Ok(())
    }

    /// Change a URL's frequency and reschedule from its last check (falls back
    /// to "due now" for never-checked URLs).
    pub fn set_schedule(&self, id: i64, schedule: Schedule, now: i64) -> Result<()> {
        let last: Option<i64> = self
            .conn
            .query_row("SELECT last_checked_at FROM urls WHERE id = ?1", [id], |r| r.get(0))
            .optional()?
            .flatten();
        let next = match last {
            Some(t) => schedule.next_from(t),
            None => now,
        };
        self.conn.execute(
            "UPDATE urls SET schedule = ?2, next_check_at = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, schedule.to_string(), next, now],
        )?;
        Ok(())
    }

    pub fn set_last_seen(&self, url_id: i64, snapshot_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE urls SET last_seen_snapshot_id = ?2 WHERE id = ?1",
            params![url_id, snapshot_id],
        )?;
        Ok(())
    }

    // ---- snapshots ---------------------------------------------------------

    pub fn latest_snapshot_meta(&self, url_id: i64) -> Result<Option<(i64, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, content_hash FROM snapshots WHERE url_id = ?1 ORDER BY id DESC LIMIT 1",
                [url_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub fn insert_snapshot(&self, url_id: i64, hash: &str, text: &str, now: i64) -> Result<i64> {
        let compressed = zstd::encode_all(text.as_bytes(), 3)?;
        self.conn.execute(
            "INSERT INTO snapshots (url_id, fetched_at, content_hash, text_zstd) \
             VALUES (?1, ?2, ?3, ?4)",
            params![url_id, now, hash, compressed],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn snapshot_text(&self, snapshot_id: i64) -> Result<String> {
        let blob: Vec<u8> = self.conn.query_row(
            "SELECT text_zstd FROM snapshots WHERE id = ?1",
            [snapshot_id],
            |r| r.get(0),
        )?;
        let bytes = zstd::decode_all(&blob[..])?;
        Ok(String::from_utf8(bytes)?)
    }

    /// Keep the newest `keep` snapshots plus anything referenced by a change
    /// row or the last-seen pointer.
    pub fn prune_snapshots(&self, url_id: i64, keep: usize) -> Result<()> {
        self.conn.execute(
            "DELETE FROM snapshots WHERE url_id = ?1 \
             AND id NOT IN (SELECT id FROM snapshots WHERE url_id = ?1 ORDER BY id DESC LIMIT ?2) \
             AND id NOT IN (SELECT from_snapshot_id FROM changes WHERE url_id = ?1) \
             AND id NOT IN (SELECT to_snapshot_id FROM changes WHERE url_id = ?1) \
             AND id NOT IN (SELECT COALESCE(last_seen_snapshot_id, -1) FROM urls WHERE id = ?1)",
            params![url_id, keep as i64],
        )?;
        Ok(())
    }

    // ---- changes -----------------------------------------------------------

    /// One unseen change per URL: extend it if it exists, else create it.
    #[allow(clippy::too_many_arguments)]
    pub fn record_change(
        &self,
        url_id: i64,
        from_snapshot_id: i64,
        to_snapshot_id: i64,
        added: usize,
        removed: usize,
        snippet: Option<&str>,
        now: i64,
    ) -> Result<()> {
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM changes WHERE url_id = ?1 AND seen_at IS NULL",
                [url_id],
                |r| r.get(0),
            )
            .optional()?;
        match existing {
            Some(change_id) => {
                self.conn.execute(
                    "UPDATE changes SET to_snapshot_id = ?2, detected_at = ?3, \
                     lines_added = ?4, lines_removed = ?5, first_change_snippet = ?6 \
                     WHERE id = ?1",
                    params![change_id, to_snapshot_id, now, added as i64, removed as i64, snippet],
                )?;
            }
            None => {
                self.conn.execute(
                    "INSERT INTO changes (url_id, detected_at, from_snapshot_id, to_snapshot_id, \
                     lines_added, lines_removed, first_change_snippet) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![url_id, now, from_snapshot_id, to_snapshot_id, added as i64, removed as i64, snippet],
                )?;
            }
        }
        Ok(())
    }

    pub fn delete_unseen_change(&self, url_id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM changes WHERE url_id = ?1 AND seen_at IS NULL",
            [url_id],
        )?;
        Ok(())
    }

    pub fn list_changes(&self, unseen_only: bool) -> Result<Vec<ChangeItem>> {
        let sql = format!(
            "{CHANGE_QUERY} {} ORDER BY (c.seen_at IS NULL) DESC, c.detected_at DESC",
            if unseen_only { "WHERE c.seen_at IS NULL" } else { "" }
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], change_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn change_by_id(&self, change_id: i64) -> Result<Option<ChangeItem>> {
        Ok(self
            .conn
            .query_row(&format!("{CHANGE_QUERY} WHERE c.id = ?1"), [change_id], change_from_row)
            .optional()?)
    }

    pub fn latest_change_for_url(&self, url_id: i64) -> Result<Option<ChangeItem>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "{CHANGE_QUERY} WHERE c.url_id = ?1 \
                     ORDER BY (c.seen_at IS NULL) DESC, c.detected_at DESC LIMIT 1"
                ),
                [url_id],
                change_from_row,
            )
            .optional()?)
    }

    /// Mark one change reviewed and advance the URL's last-seen pointer.
    pub fn mark_seen(&self, change_id: i64, now: i64) -> Result<bool> {
        let updated = self.conn.execute(
            "UPDATE changes SET seen_at = ?2 WHERE id = ?1 AND seen_at IS NULL",
            params![change_id, now],
        )?;
        if updated > 0 {
            self.conn.execute(
                "UPDATE urls SET last_seen_snapshot_id = \
                     (SELECT to_snapshot_id FROM changes WHERE id = ?1), reviewed_at = ?2 \
                 WHERE id = (SELECT url_id FROM changes WHERE id = ?1)",
                params![change_id, now],
            )?;
        }
        Ok(updated > 0)
    }

    pub fn mark_all_seen(&self, now: i64) -> Result<usize> {
        self.conn.execute(
            "UPDATE urls SET last_seen_snapshot_id = \
                 (SELECT c.to_snapshot_id FROM changes c WHERE c.url_id = urls.id AND c.seen_at IS NULL), \
                 reviewed_at = ?1 \
             WHERE id IN (SELECT url_id FROM changes WHERE seen_at IS NULL)",
            [now],
        )?;
        let n = self.conn.execute(
            "UPDATE changes SET seen_at = ?1 WHERE seen_at IS NULL",
            [now],
        )?;
        Ok(n)
    }

    pub fn count_unseen(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM changes WHERE seen_at IS NULL",
            [],
            |r| r.get(0),
        )?)
    }

    // ---- settings ----------------------------------------------------------

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    /// `now` timestamps the write so a synced preference can be merged
    /// last-write-wins. Machine-local bookkeeping keys carry one too; nobody
    /// reads it, and a uniform signature beats a second method.
    pub fn set_setting(&self, key: &str, value: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value, now],
        )?;
        Ok(())
    }

    pub fn get_setting_meta(&self, key: &str) -> Result<Option<(String, i64)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value, updated_at FROM settings WHERE key = ?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    // ---- sync support ------------------------------------------------------

    /// A watch's durable configuration — everything a sync document carries and
    /// a fresh machine needs to resume checking. Deliberately excludes derived
    /// state (`next_check_at`, `status`, `error_count`, snapshot pointers),
    /// which a restored machine rebuilds on its first check.
    pub fn upsert_url_config(&self, c: &UrlConfig<'_>, next_check_at: i64) -> Result<i64> {
        let ignores = serde_json::to_string(c.ignore_patterns)?;
        self.conn.execute(
            "INSERT INTO urls (url, title, schedule, selector, ignore_patterns, wait_selector, \
                 settle_ms, next_check_at, created_at, updated_at, reviewed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
             ON CONFLICT(url) DO UPDATE SET \
                 title = COALESCE(excluded.title, urls.title), \
                 schedule = excluded.schedule, \
                 selector = excluded.selector, \
                 ignore_patterns = excluded.ignore_patterns, \
                 wait_selector = excluded.wait_selector, \
                 settle_ms = excluded.settle_ms, \
                 updated_at = excluded.updated_at, \
                 reviewed_at = MAX(urls.reviewed_at, excluded.reviewed_at)",
            params![
                c.url,
                c.title,
                c.schedule.to_string(),
                c.selector,
                ignores,
                c.wait_selector,
                c.settle_ms.map(|v| v as i64),
                next_check_at,
                c.created_at,
                c.updated_at,
                c.reviewed_at,
            ],
        )?;
        Ok(self
            .conn
            .query_row("SELECT id FROM urls WHERE url = ?1", [c.url], |r| r.get(0))?)
    }

    pub fn set_reviewed_at(&self, url_id: i64, ts: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE urls SET reviewed_at = MAX(reviewed_at, ?2) WHERE id = ?1",
            params![url_id, ts],
        )?;
        Ok(())
    }

    /// Drop the last-reviewed pointer. Restoring a watch list without its
    /// snapshots *must* do this: `checker::persist_ok` dereferences
    /// `last_seen_snapshot_id` directly, and a pointer to a snapshot that no
    /// longer exists turns every check into a recorded error.
    pub fn clear_last_seen(&self, url_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE urls SET last_seen_snapshot_id = NULL WHERE id = ?1",
            [url_id],
        )?;
        Ok(())
    }

    pub fn unseen_change_for_url(&self, url_id: i64) -> Result<Option<ChangeItem>> {
        Ok(self
            .conn
            .query_row(
                &format!("{CHANGE_QUERY} WHERE c.url_id = ?1 AND c.seen_at IS NULL LIMIT 1"),
                [url_id],
                change_from_row,
            )
            .optional()?)
    }

    pub fn tombstones(&self) -> Result<Vec<(String, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT url, deleted_at FROM tombstones ORDER BY url")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn put_tombstone(&self, url: &str, deleted_at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tombstones (url, deleted_at) VALUES (?1, ?2) \
             ON CONFLICT(url) DO UPDATE SET deleted_at = MAX(tombstones.deleted_at, excluded.deleted_at)",
            params![url, deleted_at],
        )?;
        Ok(())
    }

    /// Forget deletes older than `before`. A tombstone only has to outlive the
    /// longest window a peer might stay offline; keeping them forever would
    /// grow the sync document without bound.
    pub fn prune_tombstones(&self, before: i64) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM tombstones WHERE deleted_at < ?1", [before])?)
    }

    pub fn unseen_url_ids(&self) -> Result<HashSet<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT url_id FROM changes WHERE seen_at IS NULL")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_store() -> Store {
        Store::open_in_memory().unwrap()
    }

    /// The migration has to be safe to run against a v0.1 database, not just a
    /// fresh one — so build a pre-sync schema by hand and put it through.
    #[test]
    fn migrates_a_pre_sync_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE urls (id INTEGER PRIMARY KEY, url TEXT NOT NULL UNIQUE, title TEXT, \
                 schedule TEXT NOT NULL DEFAULT 'weekly', selector TEXT, ignore_patterns TEXT, \
                 wait_selector TEXT, settle_ms INTEGER, next_check_at INTEGER NOT NULL, \
                 last_checked_at INTEGER, status TEXT NOT NULL DEFAULT 'ok', error_message TEXT, \
                 error_count INTEGER NOT NULL DEFAULT 0, last_seen_snapshot_id INTEGER, \
                 created_at INTEGER NOT NULL);
             CREATE TABLE changes (id INTEGER PRIMARY KEY, url_id INTEGER NOT NULL, \
                 detected_at INTEGER NOT NULL, seen_at INTEGER, from_snapshot_id INTEGER NOT NULL, \
                 to_snapshot_id INTEGER NOT NULL, lines_added INTEGER NOT NULL DEFAULT 0, \
                 lines_removed INTEGER NOT NULL DEFAULT 0, first_change_snippet TEXT);
             CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO urls (url, next_check_at, created_at) VALUES ('https://a.test', 50, 42);
             INSERT INTO changes (url_id, detected_at, seen_at, from_snapshot_id, to_snapshot_id) \
                 VALUES (1, 60, 77, 1, 2);
             INSERT INTO settings (key, value) VALUES ('digest_hour', '9');",
        )
        .unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        migrate(&conn).unwrap();
        let store = Store { conn };

        let u = &store.list_urls().unwrap()[0];
        assert_eq!(u.updated_at, 42, "seeded from created_at, not left at the epoch");
        assert_eq!(u.reviewed_at, 77, "seeded from the existing review trail");
        assert_eq!(store.get_setting_meta("digest_hour").unwrap().unwrap().0, "9");

        // Running it twice must not fail or clobber the seeded values.
        migrate(&store.conn).unwrap();
        assert_eq!(store.list_urls().unwrap()[0].reviewed_at, 77);

        // No key means nothing was syncing, so nothing gets an endpoint.
        assert_eq!(store.get_setting("sync_endpoint").unwrap(), None);
    }

    #[test]
    fn an_install_that_was_already_syncing_keeps_its_backend() {
        // Self-hosting removed the built-in endpoint. An install that was
        // already backing up must not quietly stop because of it.
        let s = mem_store();
        s.set_setting("sync_key", "SOME-KEY", 100).unwrap();
        s.set_setting("sync_endpoint", "", 100).unwrap();
        migrate(&s.conn).unwrap();
        assert_eq!(
            s.get_setting("sync_endpoint").unwrap().as_deref(),
            Some("https://sync.litecter.cc"),
        );
    }

    #[test]
    fn a_backend_the_user_chose_is_never_overwritten() {
        let s = mem_store();
        s.set_setting("sync_key", "SOME-KEY", 100).unwrap();
        s.set_setting("sync_endpoint", "https://mine.workers.dev", 100).unwrap();
        migrate(&s.conn).unwrap();
        assert_eq!(
            s.get_setting("sync_endpoint").unwrap().as_deref(),
            Some("https://mine.workers.dev"),
        );
    }

    #[test]
    fn add_resolve_remove() {
        let s = mem_store();
        let u = s.add_url("https://example.com", Schedule::Weekly, None, 100).unwrap();
        assert_eq!(u.next_check_at, 100);
        assert!(s.resolve_url("example.com").unwrap().is_some());
        assert!(s.resolve_url(&format!("{}", u.id)).unwrap().is_some());
        assert!(s.add_url("https://example.com", Schedule::Daily, None, 100).is_err());
        s.remove_url(u.id, 200).unwrap();
        assert!(s.resolve_url("example.com").unwrap().is_none());
        assert_eq!(s.tombstones().unwrap(), vec![("https://example.com".into(), 200)]);

        // Re-adding must clear the tombstone, else the next sync re-deletes it.
        s.add_url("https://example.com", Schedule::Weekly, None, 300).unwrap();
        assert!(s.tombstones().unwrap().is_empty());
    }

    #[test]
    fn snapshot_roundtrip_and_prune() {
        let s = mem_store();
        let u = s.add_url("https://example.com", Schedule::Weekly, None, 100).unwrap();
        let mut last = 0;
        for i in 0..15 {
            last = s.insert_snapshot(u.id, &format!("h{i}"), &format!("text {i}"), 100 + i).unwrap();
        }
        assert_eq!(s.snapshot_text(last).unwrap(), "text 14");
        s.prune_snapshots(u.id, 10).unwrap();
        let count: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM snapshots WHERE url_id = ?1", [u.id], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 10);
    }

    #[test]
    fn one_unseen_change_per_url_and_seen_pointer() {
        let s = mem_store();
        let u = s.add_url("https://example.com", Schedule::Weekly, None, 100).unwrap();
        let s1 = s.insert_snapshot(u.id, "h1", "one", 101).unwrap();
        let s2 = s.insert_snapshot(u.id, "h2", "two", 102).unwrap();
        let s3 = s.insert_snapshot(u.id, "h3", "three", 103).unwrap();

        s.record_change(u.id, s1, s2, 1, 1, Some("two"), 102).unwrap();
        s.record_change(u.id, s1, s3, 1, 1, Some("three"), 103).unwrap();
        let changes = s.list_changes(true).unwrap();
        assert_eq!(changes.len(), 1, "re-change extends, never piles up");
        assert_eq!(changes[0].from_snapshot_id, s1);
        assert_eq!(changes[0].to_snapshot_id, s3);

        assert!(s.mark_seen(changes[0].id, 104).unwrap());
        assert_eq!(s.count_unseen().unwrap(), 0);
        let u = s.url_by_id(u.id).unwrap().unwrap();
        assert_eq!(u.last_seen_snapshot_id, Some(s3));
    }
}
