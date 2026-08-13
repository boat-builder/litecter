//! The sync document — what actually crosses the wire, and how two machines
//! reconcile it.
//!
//! # What is worth syncing
//!
//! Litecter's database is ~99% snapshots by volume and ~1% irreplaceable state
//! by value. Snapshots are page text that a single check regenerates; the watch
//! list is hand-curated and gone forever if the disk is. So this document
//! carries the watch list, the settings, the tombstones — and, as the one
//! deliberate exception, any *unreviewed* change together with the two snapshot
//! texts it diffs. That exception is what makes a restore lossless in the way
//! users notice: your inbox survives, your read history doesn't.
//!
//! A machine restored without snapshots re-baselines silently rather than
//! reporting five hundred false changes, because `checker::persist_ok` treats a
//! URL with no snapshot as a baseline and marks it seen.
//!
//! # How two machines reconcile
//!
//! Per URL, keyed on the URL string (already `UNIQUE` in the schema, so no
//! synthetic id is needed):
//!
//! | field | rule |
//! |---|---|
//! | config (schedule, selector, filters) | last write wins on `updated_at` |
//! | `reviewed_at` | max — reviewing is monotonic, and never un-reviewing is the safe direction |
//! | `pending` | taken wholesale from whichever side checked most recently (`pending_at`) |
//! | deletion | tombstone wins if `deleted_at` is newer than `updated_at` |
//!
//! Resolving `pending` by *who checked last* rather than by "who has one" is
//! what makes a revert propagate: the machine that saw the page return to its
//! reviewed state has the newest `pending_at` and a `pending` of `None`, so its
//! absence wins over the other machine's stale change.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::schedule::Schedule;
use crate::store::{Store, UrlConfig};

pub const DOC_VERSION: u32 = 1;

/// Deletes are forgotten after this long. It only has to exceed the longest
/// plausible window a second machine stays offline; past that, a resurrected
/// URL is a smaller sin than an unbounded document.
pub const TOMBSTONE_TTL_SECS: i64 = 180 * 24 * 60 * 60;

/// Settings that describe *the user*, not *the machine*. `last_digest_date`,
/// `autostart_initialized` and everything under `sync_` are deliberately absent:
/// they are local bookkeeping and syncing them would make machines fight.
pub const SYNCED_SETTINGS: &[&str] = &["digest_hour"];

/// Keep the sealed upload comfortably under the endpoint's 8 MB ceiling. When a
/// document exceeds this, snapshot texts are shed (see [`SyncDoc::trim_to`]).
pub const SOFT_BUDGET_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncDoc {
    pub v: u32,
    pub updated_at: i64,
    #[serde(default)]
    pub urls: Vec<SyncUrl>,
    #[serde(default)]
    pub tombstones: Vec<Tombstone>,
    #[serde(default)]
    pub settings: BTreeMap<String, SyncSetting>,
}

impl Default for SyncDoc {
    fn default() -> Self {
        Self {
            v: DOC_VERSION,
            updated_at: 0,
            urls: Vec::new(),
            tombstones: Vec::new(),
            settings: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncUrl {
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    pub schedule: String,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub ignore_patterns: Vec<String>,
    #[serde(default)]
    pub wait_selector: Option<String>,
    #[serde(default)]
    pub settle_ms: Option<u64>,
    pub created_at: i64,
    /// Last *config* change. Drives last-write-wins.
    pub updated_at: i64,
    /// Last time the user marked this URL reviewed.
    pub reviewed_at: i64,
    /// Last time this machine checked the page — decides who is authoritative
    /// about `pending`.
    #[serde(default)]
    pub pending_at: i64,
    #[serde(default)]
    pub pending: Option<SyncPending>,
}

/// An unreviewed change, carried with enough text to render its diff on a
/// machine that has never seen the page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPending {
    pub detected_at: i64,
    pub lines_added: i64,
    pub lines_removed: i64,
    #[serde(default)]
    pub snippet: Option<String>,
    /// Both texts, or neither — a diff needs the pair. Dropped under budget
    /// pressure, in which case the URL simply re-baselines on its next check.
    #[serde(default)]
    pub from_text: Option<String>,
    #[serde(default)]
    pub to_text: Option<String>,
}

impl SyncPending {
    fn texts(&self) -> Option<(&str, &str)> {
        match (&self.from_text, &self.to_text) {
            (Some(f), Some(t)) => Some((f.as_str(), t.as_str())),
            _ => None,
        }
    }

    fn text_bytes(&self) -> usize {
        self.from_text.as_ref().map_or(0, |s| s.len()) + self.to_text.as_ref().map_or(0, |s| s.len())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tombstone {
    pub url: String,
    pub deleted_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSetting {
    pub value: String,
    pub updated_at: i64,
}

impl SyncDoc {
    /// Read the durable slice of the local database.
    pub fn build(store: &Store, now: i64) -> Result<Self> {
        let mut urls = Vec::new();
        for u in store.list_urls()? {
            let pending = match store.unseen_change_for_url(u.id)? {
                Some(c) => {
                    // Missing snapshot text is not fatal — a pruned or
                    // half-restored row still carries its metadata forward.
                    let from_text = store.snapshot_text(c.from_snapshot_id).ok();
                    let to_text = store.snapshot_text(c.to_snapshot_id).ok();
                    Some(SyncPending {
                        detected_at: c.detected_at,
                        lines_added: c.lines_added,
                        lines_removed: c.lines_removed,
                        snippet: c.snippet,
                        from_text,
                        to_text,
                    })
                }
                None => None,
            };
            urls.push(SyncUrl {
                url: u.url,
                title: u.title,
                schedule: u.schedule.to_string(),
                selector: u.selector,
                ignore_patterns: u.ignore_patterns,
                wait_selector: u.wait_selector,
                settle_ms: u.settle_ms,
                created_at: u.created_at,
                updated_at: u.updated_at,
                reviewed_at: u.reviewed_at,
                pending_at: u.last_checked_at.unwrap_or(0),
                pending,
            });
        }

        let mut settings = BTreeMap::new();
        for key in SYNCED_SETTINGS {
            if let Some((value, updated_at)) = store.get_setting_meta(key)? {
                settings.insert((*key).to_string(), SyncSetting { value, updated_at });
            }
        }

        Ok(Self {
            v: DOC_VERSION,
            updated_at: now,
            urls,
            tombstones: store
                .tombstones()?
                .into_iter()
                .map(|(url, deleted_at)| Tombstone { url, deleted_at })
                .collect(),
            settings,
        })
    }

    /// Shed snapshot texts, biggest first, until the document fits `budget`.
    /// Returns how many pendings lost their diff. Metadata is always kept, so
    /// the user still sees *that* something changed.
    pub fn trim_to(&mut self, budget: usize) -> Result<usize> {
        let mut dropped = 0;
        loop {
            if serde_json::to_vec(&self)?.len() <= budget {
                return Ok(dropped);
            }
            let fattest = self
                .urls
                .iter_mut()
                .filter_map(|u| u.pending.as_mut())
                .filter(|p| p.text_bytes() > 0)
                .max_by_key(|p| p.text_bytes());
            match fattest {
                Some(p) => {
                    p.from_text = None;
                    p.to_text = None;
                    dropped += 1;
                }
                // Nothing left to shed: the watch list alone exceeds the
                // budget. Push it anyway — the endpoint's own limit is 2× this.
                None => return Ok(dropped),
            }
        }
    }
}

/// Reconcile two documents. Pure and order-independent: `merge(a, b)` and
/// `merge(b, a)` agree except where timestamps tie, which local wins by
/// convention so a machine never appears to lose its own edit.
pub fn merge(local: SyncDoc, remote: SyncDoc) -> SyncDoc {
    let horizon = local.updated_at.max(remote.updated_at);

    let mut graves: BTreeMap<String, i64> = BTreeMap::new();
    for t in local.tombstones.into_iter().chain(remote.tombstones) {
        let slot = graves.entry(t.url).or_insert(t.deleted_at);
        *slot = (*slot).max(t.deleted_at);
    }

    let mut urls: BTreeMap<String, SyncUrl> = BTreeMap::new();
    for incoming in local.urls.into_iter().chain(remote.urls) {
        match urls.remove(&incoming.url) {
            None => {
                urls.insert(incoming.url.clone(), incoming);
            }
            Some(existing) => {
                urls.insert(incoming.url.clone(), reconcile(existing, incoming));
            }
        }
    }

    // A delete only sticks if nothing touched the URL afterwards.
    urls.retain(|url, u| graves.get(url).is_none_or(|&died| died <= u.updated_at));

    let mut settings = local.settings;
    for (key, incoming) in remote.settings {
        settings
            .entry(key)
            .and_modify(|held| {
                if incoming.updated_at > held.updated_at {
                    *held = incoming.clone();
                }
            })
            .or_insert(incoming);
    }

    SyncDoc {
        v: DOC_VERSION,
        updated_at: horizon,
        urls: urls.into_values().collect(),
        tombstones: graves
            .into_iter()
            .filter(|(_, died)| *died >= horizon - TOMBSTONE_TTL_SECS)
            .map(|(url, deleted_at)| Tombstone { url, deleted_at })
            .collect(),
        settings,
    }
}

/// Merge two views of the same URL. `a` is the earlier-seen side (local when
/// called from [`merge`]), which is why ties resolve to it.
fn reconcile(a: SyncUrl, b: SyncUrl) -> SyncUrl {
    let (mut winner, loser) = if b.updated_at > a.updated_at { (b, a) } else { (a, b) };

    winner.created_at = winner.created_at.min(loser.created_at);
    winner.reviewed_at = winner.reviewed_at.max(loser.reviewed_at);
    // A title is cosmetic and re-derived on every check, so take whichever
    // side actually has one rather than letting a config edit blank it.
    if winner.title.is_none() {
        winner.title = loser.title;
    }

    // Whoever checked the page last is authoritative about whether a change is
    // outstanding — including when the answer is "no longer".
    if loser.pending_at > winner.pending_at {
        winner.pending_at = loser.pending_at;
        winner.pending = loser.pending;
    }

    // Reviewed elsewhere, after this change was detected: it's done.
    if let Some(p) = &winner.pending
        && p.detected_at <= winner.reviewed_at
    {
        winner.pending = None;
    }
    winner
}

/// Write a merged document into the local database.
///
/// Returns a summary for the caller to report. `now` dates newly scheduled
/// checks; restored URLs become due immediately so a fresh machine builds its
/// baselines rather than sitting idle until the first interval elapses.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ApplyStats {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub pendings_restored: usize,
}

pub fn apply(store: &Store, doc: &SyncDoc, now: i64) -> Result<ApplyStats> {
    let mut stats = ApplyStats::default();

    // Learn about deletes first, so a URL deleted remotely isn't briefly
    // re-created and re-checked.
    for grave in &doc.tombstones {
        store.put_tombstone(&grave.url, grave.deleted_at)?;
        if let Some(existing) = store.resolve_url(&grave.url)? {
            // Preserve the original delete time rather than stamping it now,
            // or the tombstone would keep refreshing itself forever.
            store.remove_url(existing.id, grave.deleted_at)?;
            stats.removed += 1;
        }
    }

    for u in &doc.urls {
        let existed = store.resolve_url(&u.url)?;
        let schedule: Schedule = u.schedule.parse().unwrap_or(Schedule::Weekly);
        let next_check_at = existed.as_ref().map_or(now, |e| e.next_check_at);

        let id = store
            .upsert_url_config(
                &UrlConfig {
                    url: &u.url,
                    title: u.title.as_deref(),
                    schedule,
                    selector: u.selector.as_deref(),
                    ignore_patterns: &u.ignore_patterns,
                    wait_selector: u.wait_selector.as_deref(),
                    settle_ms: u.settle_ms,
                    created_at: u.created_at,
                    updated_at: u.updated_at,
                    reviewed_at: u.reviewed_at,
                },
                next_check_at,
            )
            .with_context(|| format!("restoring {}", u.url))?;

        match existed {
            Some(_) => stats.updated += 1,
            None => {
                // A brand-new row has no snapshots, so it must not inherit a
                // last-seen pointer from anywhere: `checker::persist_ok`
                // dereferences it and would record an error on every check.
                store.clear_last_seen(id)?;
                stats.added += 1;
            }
        }

        if restore_pending(store, id, u)? {
            stats.pendings_restored += 1;
        }
    }

    store.prune_tombstones(now - TOMBSTONE_TTL_SECS)?;
    Ok(stats)
}

/// Bring the local inbox in line with the merged document for one URL.
/// Returns whether a change was materialized from remote text.
fn restore_pending(store: &Store, url_id: i64, u: &SyncUrl) -> Result<bool> {
    let local = store.unseen_change_for_url(url_id)?;
    match (&u.pending, local) {
        // Already in step — this is the common case, including when the
        // document was built from this very machine.
        (Some(p), Some(existing)) if existing.detected_at == p.detected_at => Ok(false),

        (Some(p), _) => {
            let Some((from_text, to_text)) = p.texts() else {
                // Text was shed under budget pressure. Leave the URL to
                // re-detect on its next check rather than record a change
                // whose diff can't be rendered.
                return Ok(false);
            };
            let from_id =
                store.insert_snapshot(url_id, &crate::textproc::hash(from_text), from_text, p.detected_at)?;
            let to_id =
                store.insert_snapshot(url_id, &crate::textproc::hash(to_text), to_text, p.detected_at)?;
            store.record_change(
                url_id,
                from_id,
                to_id,
                p.lines_added as usize,
                p.lines_removed as usize,
                p.snippet.as_deref(),
                p.detected_at,
            )?;
            // The diff reads last-reviewed → latest, so the pointer belongs on
            // the *from* side.
            store.set_last_seen(url_id, from_id)?;
            Ok(true)
        }

        // Reviewed or reverted elsewhere.
        (None, Some(_)) => {
            store.delete_unseen_change(url_id)?;
            store.set_reviewed_at(url_id, u.reviewed_at)?;
            Ok(false)
        }

        (None, None) => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checker::{persist_check, CheckResult, Fetched};
    use crate::store::Store;

    fn url(name: &str, updated_at: i64) -> SyncUrl {
        SyncUrl {
            url: format!("https://{name}.test"),
            title: None,
            schedule: "weekly".into(),
            selector: None,
            ignore_patterns: Vec::new(),
            wait_selector: None,
            settle_ms: None,
            created_at: 1,
            updated_at,
            reviewed_at: 0,
            pending_at: 0,
            pending: None,
        }
    }

    fn pending(detected_at: i64) -> SyncPending {
        SyncPending {
            detected_at,
            lines_added: 2,
            lines_removed: 1,
            snippet: Some("the new line".into()),
            from_text: Some("before\nshared".into()),
            to_text: Some("after\nshared\nextra".into()),
        }
    }

    fn doc(urls: Vec<SyncUrl>, updated_at: i64) -> SyncDoc {
        SyncDoc { updated_at, urls, ..Default::default() }
    }

    fn names(d: &SyncDoc) -> Vec<&str> {
        d.urls.iter().map(|u| u.url.as_str()).collect()
    }

    // ---- merge -------------------------------------------------------------

    #[test]
    fn union_of_both_watch_lists() {
        let merged = merge(doc(vec![url("a", 10)], 10), doc(vec![url("b", 10)], 10));
        assert_eq!(names(&merged), ["https://a.test", "https://b.test"]);
    }

    #[test]
    fn newer_config_wins() {
        let mut old = url("a", 10);
        old.schedule = "weekly".into();
        let mut new = url("a", 20);
        new.schedule = "daily".into();

        assert_eq!(merge(doc(vec![old.clone()], 10), doc(vec![new.clone()], 20)).urls[0].schedule, "daily");
        // …regardless of which side it arrives from.
        assert_eq!(merge(doc(vec![new], 20), doc(vec![old], 10)).urls[0].schedule, "daily");
    }

    #[test]
    fn merge_is_order_independent() {
        let a = doc(vec![url("a", 30), url("shared", 10)], 30);
        let b = doc(vec![url("b", 20), url("shared", 25)], 25);
        let forward = merge(a.clone(), b.clone());
        let backward = merge(b, a);
        assert_eq!(names(&forward), names(&backward));
        assert_eq!(forward.urls[2].updated_at, backward.urls[2].updated_at);
    }

    #[test]
    fn a_delete_removes_a_stale_peer_copy() {
        let alive = doc(vec![url("a", 10)], 10);
        let deleted = SyncDoc {
            updated_at: 50,
            tombstones: vec![Tombstone { url: "https://a.test".into(), deleted_at: 40 }],
            ..Default::default()
        };
        assert!(merge(alive, deleted).urls.is_empty(), "delete is newer than the last edit");
    }

    #[test]
    fn re_adding_after_a_delete_beats_the_tombstone() {
        let readded = doc(vec![url("a", 60)], 60);
        let deleted = SyncDoc {
            updated_at: 50,
            tombstones: vec![Tombstone { url: "https://a.test".into(), deleted_at: 40 }],
            ..Default::default()
        };
        assert_eq!(merge(readded, deleted).urls.len(), 1, "the edit is newer than the delete");
    }

    #[test]
    fn ancient_tombstones_are_forgotten() {
        let now = 1_800_000_000; // a plausible wall clock, so the TTL is meaningful
        let recent = Tombstone { url: "https://new.test".into(), deleted_at: now - 1000 };
        let ancient =
            Tombstone { url: "https://old.test".into(), deleted_at: now - TOMBSTONE_TTL_SECS - 1 };
        let merged = merge(
            SyncDoc { updated_at: now, tombstones: vec![recent, ancient], ..Default::default() },
            SyncDoc::default(),
        );
        assert_eq!(merged.tombstones.len(), 1, "one is older than the TTL");
        assert_eq!(merged.tombstones[0].url, "https://new.test");
    }

    #[test]
    fn reviewing_on_one_machine_clears_the_pending_on_the_other() {
        let mut unreviewed = url("a", 10);
        unreviewed.pending_at = 100;
        unreviewed.pending = Some(pending(100));

        let mut reviewed = url("a", 10);
        reviewed.reviewed_at = 200; // reviewed after the change was detected
        reviewed.pending_at = 90;

        let merged = merge(doc(vec![unreviewed], 100), doc(vec![reviewed], 200));
        assert!(merged.urls[0].pending.is_none(), "already reviewed elsewhere");
        assert_eq!(merged.urls[0].reviewed_at, 200);
    }

    #[test]
    fn a_change_detected_after_review_survives() {
        let mut fresh = url("a", 10);
        fresh.pending_at = 300;
        fresh.pending = Some(pending(300));

        let mut reviewed = url("a", 10);
        reviewed.reviewed_at = 200;
        reviewed.pending_at = 200;

        let merged = merge(doc(vec![fresh], 300), doc(vec![reviewed], 200));
        assert!(merged.urls[0].pending.is_some(), "detected after the last review");
    }

    #[test]
    fn a_revert_propagates_as_an_absence() {
        // The whole reason `pending` follows `pending_at` rather than
        // "whoever has one": the machine that saw the page revert has no
        // pending, and its silence has to win.
        let mut stale = url("a", 10);
        stale.pending_at = 100;
        stale.pending = Some(pending(100));

        let mut reverted = url("a", 10);
        reverted.pending_at = 500; // checked later, found nothing outstanding

        let merged = merge(doc(vec![stale], 100), doc(vec![reverted], 500));
        assert!(merged.urls[0].pending.is_none(), "the later check wins");
    }

    #[test]
    fn a_title_is_never_blanked_by_a_config_edit() {
        let mut titled = url("a", 10);
        titled.title = Some("Stripe API".into());
        let untitled = url("a", 20); // newer, but never checked so has no title

        assert_eq!(
            merge(doc(vec![titled], 10), doc(vec![untitled], 20)).urls[0].title.as_deref(),
            Some("Stripe API")
        );
    }

    #[test]
    fn settings_merge_last_write_wins() {
        let mut older = SyncDoc::default();
        older.settings.insert("digest_hour".into(), SyncSetting { value: "9".into(), updated_at: 10 });
        let mut newer = SyncDoc::default();
        newer.settings.insert("digest_hour".into(), SyncSetting { value: "17".into(), updated_at: 20 });

        assert_eq!(merge(older.clone(), newer.clone()).settings["digest_hour"].value, "17");
        assert_eq!(merge(newer, older).settings["digest_hour"].value, "17");
    }

    // ---- trimming ----------------------------------------------------------

    #[test]
    fn trimming_sheds_diffs_and_keeps_the_watch_list() {
        let mut d = doc((0..5).map(|i| url(&format!("u{i}"), 10)).collect(), 10);
        for (i, u) in d.urls.iter_mut().enumerate() {
            let mut p = pending(100);
            p.from_text = Some("x".repeat(2000 * (i + 1)));
            p.to_text = Some("y".repeat(2000 * (i + 1)));
            u.pending = Some(p);
        }

        let dropped = d.trim_to(8_000).unwrap();
        assert!(dropped > 0);
        assert_eq!(d.urls.len(), 5, "the watch list itself is never shed");
        // Metadata outlives the text, so the user still sees that it changed.
        let stripped = d.urls.iter().filter(|u| u.pending.as_ref().is_some_and(|p| p.texts().is_none()));
        assert!(stripped.count() > 0);
        assert!(d.urls.iter().all(|u| u.pending.is_some()), "counts survive");
    }

    // ---- apply -------------------------------------------------------------

    #[test]
    fn a_fresh_machine_gets_the_watch_list_and_the_inbox() {
        let store = Store::open_in_memory().unwrap();
        let mut with_change = url("a", 10);
        with_change.pending_at = 100;
        with_change.pending = Some(pending(100));

        let stats = apply(&store, &doc(vec![with_change, url("b", 10)], 100), 1_000).unwrap();
        assert_eq!(stats.added, 2);
        assert_eq!(stats.pendings_restored, 1);

        assert_eq!(store.list_urls().unwrap().len(), 2);
        let inbox = store.list_changes(true).unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].url, "https://a.test");
        assert_eq!(inbox[0].lines_added, 2);

        // The restored diff must actually render.
        let from = store.snapshot_text(inbox[0].from_snapshot_id).unwrap();
        let to = store.snapshot_text(inbox[0].to_snapshot_id).unwrap();
        assert_eq!(from, "before\nshared");
        assert_eq!(to, "after\nshared\nextra");
    }

    #[test]
    fn a_restored_url_without_snapshots_baselines_instead_of_erroring() {
        // The trap: `checker::persist_ok` dereferences last_seen_snapshot_id
        // directly. A restored row pointing at a snapshot id that no longer
        // exists turns every future check into a recorded error.
        let store = Store::open_in_memory().unwrap();
        apply(&store, &doc(vec![url("a", 10)], 100), 1_000).unwrap();

        let row = store.resolve_url("https://a.test").unwrap().unwrap();
        assert_eq!(row.last_seen_snapshot_id, None, "must not inherit a dangling pointer");

        let fetched = Fetched {
            text: "page text".into(),
            hash: crate::textproc::hash("page text"),
            title: Some("A".into()),
        };
        match persist_check(&store, &row, Ok(fetched), 2_000) {
            CheckResult::Baseline => {}
            other => panic!("expected a silent baseline, got {other:?}"),
        }
        assert_eq!(store.count_unseen().unwrap(), 0, "a restore must not fake 500 changes");
    }

    #[test]
    fn a_restored_pending_still_diffs_against_the_next_check() {
        let store = Store::open_in_memory().unwrap();
        let mut u = url("a", 10);
        u.pending_at = 100;
        u.pending = Some(pending(100));
        apply(&store, &doc(vec![u], 100), 1_000).unwrap();

        // A further change on top of a restored inbox item extends it, and the
        // diff still reads from what was last reviewed.
        let row = store.resolve_url("https://a.test").unwrap().unwrap();
        let text = "after\nshared\nextra\nmore";
        let fetched = Fetched { text: text.into(), hash: crate::textproc::hash(text), title: None };
        match persist_check(&store, &row, Ok(fetched), 2_000) {
            CheckResult::Changed { .. } => {}
            other => panic!("expected a change, got {other:?}"),
        }
        let inbox = store.list_changes(true).unwrap();
        assert_eq!(inbox.len(), 1, "extends the existing item, never piles up");
        assert_eq!(store.snapshot_text(inbox[0].from_snapshot_id).unwrap(), "before\nshared");
    }

    #[test]
    fn applying_a_delete_removes_the_local_copy() {
        let store = Store::open_in_memory().unwrap();
        store.add_url("https://a.test", Schedule::Weekly, None, 10).unwrap();

        let d = SyncDoc {
            updated_at: 100,
            tombstones: vec![Tombstone { url: "https://a.test".into(), deleted_at: 50 }],
            ..Default::default()
        };
        let stats = apply(&store, &d, 1_000).unwrap();
        assert_eq!(stats.removed, 1);
        assert!(store.resolve_url("https://a.test").unwrap().is_none());
        // The tombstone keeps its original date rather than refreshing itself,
        // so it can eventually age out.
        assert_eq!(store.tombstones().unwrap(), vec![("https://a.test".into(), 50)]);
    }

    #[test]
    fn a_full_round_trip_against_itself_changes_nothing() {
        let store = Store::open_in_memory().unwrap();
        let u = store.add_url("https://a.test", Schedule::Daily, Some("main"), 10).unwrap();
        let s1 = store.insert_snapshot(u.id, "h1", "one", 20).unwrap();
        let s2 = store.insert_snapshot(u.id, "h2", "two", 30).unwrap();
        store.set_last_seen(u.id, s1).unwrap();
        store.record_change(u.id, s1, s2, 1, 1, Some("two"), 30).unwrap();
        store.update_check_ok(u.id, Some("A"), 30, 40).unwrap();

        let before = SyncDoc::build(&store, 100).unwrap();
        let merged = merge(before.clone(), SyncDoc::default());
        apply(&store, &merged, 100).unwrap();
        let after = SyncDoc::build(&store, 100).unwrap();

        assert_eq!(store.list_urls().unwrap().len(), 1);
        assert_eq!(store.list_changes(true).unwrap().len(), 1, "no duplicate inbox item");
        assert_eq!(before.urls[0].url, after.urls[0].url);
        assert_eq!(before.urls[0].schedule, after.urls[0].schedule);
        assert_eq!(before.urls[0].selector, after.urls[0].selector);
        assert_eq!(
            before.urls[0].pending.as_ref().unwrap().detected_at,
            after.urls[0].pending.as_ref().unwrap().detected_at,
        );
    }

    #[test]
    fn build_captures_the_settings_whitelist_only() {
        let store = Store::open_in_memory().unwrap();
        store.set_setting("digest_hour", "17", 50).unwrap();
        store.set_setting("last_digest_date", "2026-08-13", 50).unwrap();
        store.set_setting("sync_etag", "abc", 50).unwrap();

        let d = SyncDoc::build(&store, 100).unwrap();
        assert_eq!(d.settings.len(), 1, "machine-local keys must not travel");
        assert_eq!(d.settings["digest_hour"].value, "17");
    }
}
