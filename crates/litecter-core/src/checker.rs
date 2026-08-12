use anyhow::Result;

use crate::differ;
use crate::renderer::Renderer;
use crate::store::{Store, UrlRow};
use crate::textproc;

const SNAPSHOTS_KEPT: usize = 10;

#[derive(Debug)]
pub enum CheckResult {
    /// Same content as the latest snapshot.
    Unchanged,
    /// First ever snapshot — the baseline, counts as seen.
    Baseline,
    /// Content differs from what the user last reviewed.
    Changed { added: usize, removed: usize },
    /// Page went back to exactly the last-reviewed state; pending change dropped.
    Reverted,
    Errored(String),
}

/// The browser half of a check: render + normalize + hash. Touches no store,
/// so callers that share a Store behind a lock can run this unlocked.
pub struct Fetched {
    pub text: String,
    pub hash: String,
    pub title: Option<String>,
}

pub async fn fetch_rendered(renderer: &Renderer, u: &UrlRow) -> Result<Fetched> {
    let rendered = renderer
        .render(&u.url, u.selector.as_deref(), u.wait_selector.as_deref(), u.settle_ms)
        .await?;
    let ignores = textproc::compile_ignores(&u.ignore_patterns);
    let text = textproc::normalize(&rendered.text, &ignores);
    let hash = textproc::hash(&text);
    Ok(Fetched { text, hash, title: rendered.title })
}

/// The store half of a check: all fast, synchronous SQLite work.
pub fn persist_check(store: &Store, u: &UrlRow, fetched: Result<Fetched>, now: i64) -> CheckResult {
    match fetched.and_then(|f| persist_ok(store, u, f, now)) {
        Ok(result) => result,
        Err(e) => {
            let msg = format!("{e:#}");
            let retry_at = error_backoff(u, now);
            let _ = store.update_check_err(u.id, &msg, now, retry_at);
            CheckResult::Errored(msg)
        }
    }
}

/// Run one check end-to-end and persist the outcome. Never propagates an
/// error: failures are recorded on the URL row (with backoff) and returned
/// as `Errored`.
pub async fn check_one(store: &Store, renderer: &Renderer, u: &UrlRow, now: i64) -> CheckResult {
    let fetched = fetch_rendered(renderer, u).await;
    persist_check(store, u, fetched, now)
}

/// 5 min × 2^failures, capped at the URL's normal interval.
fn error_backoff(u: &UrlRow, now: i64) -> i64 {
    let exp = u.error_count.clamp(0, 10) as u32;
    let delay = 300i64.saturating_mul(1i64 << exp).min(u.schedule.interval_secs());
    now + delay
}

fn persist_ok(store: &Store, u: &UrlRow, f: Fetched, now: i64) -> Result<CheckResult> {
    let next = u.schedule.next_from(now);
    let title = f.title.as_deref();

    let latest = store.latest_snapshot_meta(u.id)?;
    if let Some((_, latest_hash)) = &latest {
        if *latest_hash == f.hash {
            store.update_check_ok(u.id, title, now, next)?;
            return Ok(CheckResult::Unchanged);
        }
    }

    let snap_id = store.insert_snapshot(u.id, &f.hash, &f.text, now)?;

    let result = match latest {
        None => {
            store.set_last_seen(u.id, snap_id)?;
            CheckResult::Baseline
        }
        Some((latest_id, _)) => {
            // Diff against what the user last reviewed, not the previous fetch —
            // one cumulative unseen item per URL, never a pile.
            let from_id = u.last_seen_snapshot_id.unwrap_or(latest_id);
            let from_text = store.snapshot_text(from_id)?;
            if textproc::hash(&from_text) == f.hash {
                store.delete_unseen_change(u.id)?;
                CheckResult::Reverted
            } else {
                let stats = differ::stats(&from_text, &f.text);
                store.record_change(
                    u.id,
                    from_id,
                    snap_id,
                    stats.added,
                    stats.removed,
                    stats.snippet.as_deref(),
                    now,
                )?;
                CheckResult::Changed { added: stats.added, removed: stats.removed }
            }
        }
    };

    store.prune_snapshots(u.id, SNAPSHOTS_KEPT)?;
    store.update_check_ok(u.id, title, now, next)?;
    Ok(result)
}
