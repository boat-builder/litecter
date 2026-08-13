//! Cloud sync — the answer to "what if I lose this machine".
//!
//! Litecter stays local-first: SQLite remains the only thing the UI reads, so
//! the app is instant and works offline. Sync is a background errand that keeps
//! a copy of the *irreplaceable* slice of the database in Cloudflare R2, sealed
//! with a key the server never sees.
//!
//! One round is: pull → merge → apply locally → push. The merge is what makes
//! this safe on two machines; without it, "back up the database" would mean the
//! last machine to run silently discards the other's work.
//!
//! ```text
//!   ┌── pull ──► sealed bytes ──► open ──► remote doc ─┐
//!   │                                                   ├─► merge ─┬─► apply to SQLite
//!   └── build from SQLite ──────────────► local doc ───┘           └─► seal ─► push (If-Match)
//! ```
//!
//! A 412 on push means another device wrote in between; the whole round runs
//! again against the new document. That is a real convergence loop rather than
//! a retry, so each attempt starts from strictly newer state.

pub mod client;
pub mod crypto;
pub mod doc;
pub mod key;
pub mod link;
pub mod setup;
pub mod worker;

use anyhow::{Context, Result};

pub use client::{SyncClient, WorkerMeta};
pub use doc::{ApplyStats, SyncDoc};
pub use key::SyncKey;
pub use worker::WorkerCheck;

use crate::store::Store;

/// Settings keys holding sync state. Named with a shared prefix so
/// [`doc::SYNCED_SETTINGS`] can never accidentally include them — syncing your
/// own sync bookkeeping would be a fine way to build an infinite loop.
pub const KEY_SETTING: &str = "sync_key";
pub const ETAG_SETTING: &str = "sync_etag";
pub const LAST_SYNC_SETTING: &str = "sync_last_at";
pub const ENDPOINT_SETTING: &str = "sync_endpoint";
pub const FAILING_SINCE_SETTING: &str = "sync_failing_since";
pub const LAST_ERROR_SETTING: &str = "sync_last_error";
pub const FAILURE_NOTIFIED_SETTING: &str = "sync_failure_notified";

/// How long a backup must be failing before it is worth interrupting the user.
/// A backup that is a few hours stale is not news; one that has not worked in a
/// day means something needs attention.
pub const FAILURE_NOTICE_AFTER_SECS: i64 = 24 * 60 * 60;

/// How many times to re-run the round when another device keeps winning the
/// race. Three is generous: each loss means a concurrent write, and two
/// machines rarely collide even once.
const MAX_ATTEMPTS: usize = 3;

#[derive(Debug, Default)]
pub struct SyncReport {
    pub stats: ApplyStats,
    pub urls_in_document: usize,
    pub uploaded_bytes: usize,
    pub diffs_dropped_for_size: usize,
    pub attempts: usize,
}

/// Whether this machine is set up to sync.
///
/// Both halves are required. A key on its own is what setup leaves behind
/// between "show me the token" and "here is where I deployed it", and treating
/// that half-state as configured would have the scheduler trying to sync to
/// nowhere every minute.
pub fn is_configured(store: &Store) -> Result<bool> {
    Ok(load_key(store)?.is_some() && endpoint(store)?.is_some())
}

pub fn load_key(store: &Store) -> Result<Option<SyncKey>> {
    // Settings are cleared by blanking the value, not by dropping the row, so
    // "no key" arrives here as `Some("")` as often as it does as `None`.
    match store.get_setting(KEY_SETTING)?.filter(|e| !e.trim().is_empty()) {
        Some(encoded) => Ok(Some(SyncKey::decode(&encoded).context(
            "the stored sync key is unreadable — set it again with `litecter sync key --set`",
        )?)),
        None => Ok(None),
    }
}

/// Persist a key, replacing any existing one. Changing keys abandons the old
/// document, so the ETag has to go with it.
pub fn save_key(store: &Store, key: &SyncKey, now: i64) -> Result<()> {
    store.set_setting(KEY_SETTING, &key.encode(), now)?;
    store.set_setting(ETAG_SETTING, "", now)?;
    Ok(())
}

/// Return the existing key, or create and store one on first use.
pub fn ensure_key(store: &Store, now: i64) -> Result<SyncKey> {
    if let Some(key) = load_key(store)? {
        return Ok(key);
    }
    let key = SyncKey::generate()?;
    save_key(store, &key, now)?;
    Ok(key)
}

/// Where this machine's backend lives, if one has been connected.
///
/// There is deliberately no default. Litecter does not run a sync service —
/// every user deploys [their own backend](worker), so an endpoint we invented
/// would either be a server we are quietly paying for or an address that does
/// not exist.
pub fn endpoint(store: &Store) -> Result<Option<String>> {
    Ok(store.get_setting(ENDPOINT_SETTING)?.filter(|e| !e.trim().is_empty()))
}

/// Normalise and store a backend address. Returns what was actually saved.
pub fn save_endpoint(store: &Store, raw: &str, now: i64) -> Result<String> {
    let normalised = link::normalize_endpoint(raw)?;
    // Moving to a different backend means a different document, so the ETag
    // from the old one would make the first push fail a precondition it has no
    // business being judged against.
    if endpoint(store)?.as_deref() != Some(normalised.as_str()) {
        store.set_setting(ETAG_SETTING, "", now)?;
    }
    store.set_setting(ENDPOINT_SETTING, &normalised, now)?;
    Ok(normalised)
}

/// Forget this machine's connection, leaving the local database untouched.
///
/// Deliberately does not delete the remote document: this is "stop syncing",
/// and erasing someone's backup as a side effect of disconnecting is the kind
/// of helpfulness nobody asks for twice.
pub fn disconnect(store: &Store, now: i64) -> Result<()> {
    for setting in [KEY_SETTING, ETAG_SETTING, LAST_SYNC_SETTING, ENDPOINT_SETTING] {
        store.set_setting(setting, "", now)?;
    }
    clear_failure(store, now)
}

/// One paste that carries a whole connection to another machine.
pub fn link_code(store: &Store) -> Result<Option<String>> {
    match (load_key(store)?, endpoint(store)?) {
        (Some(key), Some(endpoint)) => Ok(Some(link::encode(&key, &endpoint))),
        _ => Ok(None),
    }
}

/// Adopt a connection from another machine, in either the combined form or as a
/// bare key. Verify it with [`Connection::verify`] before calling this.
pub fn adopt_link(store: &Store, raw: &str, now: i64) -> Result<()> {
    let parsed = link::parse(raw)?;
    save_key(store, &parsed.key, now)?;
    if let Some(endpoint) = parsed.endpoint {
        save_endpoint(store, &endpoint, now)?;
    }
    Ok(())
}

/// Both halves of a connection, read out of the store together.
///
/// Existing as a value at all is the point: every network call below needs a
/// key and an address, and lifting them out in one go is what lets a caller
/// drop its database lock before it starts waiting on Cloudflare.
pub struct Connection {
    pub endpoint: String,
    pub key: SyncKey,
}

impl Connection {
    pub fn load(store: &Store) -> Result<Option<Self>> {
        match (endpoint(store)?, load_key(store)?) {
            (Some(endpoint), Some(key)) => Ok(Some(Self { endpoint, key })),
            _ => Ok(None),
        }
    }

    fn client(&self) -> Result<SyncClient> {
        SyncClient::new(&self.endpoint, self.key.auth_token())
    }

    /// Prove a connection works before saving it. Setup ends here.
    pub async fn verify(&self) -> Result<WorkerMeta> {
        self.client()?.verify().await
    }

    /// Erase the backup itself. Step one of removing a backend, because the
    /// bucket cannot be deleted while it still holds this object.
    pub async fn erase(&self) -> Result<()> {
        self.client()?.delete().await
    }

    /// Ask the deployment what version it runs and compare it with ours.
    pub async fn probe(&self) -> Result<WorkerCheck> {
        let meta = self.client()?.meta().await?;
        Ok(WorkerCheck {
            deployed: meta.version,
            bundled: worker::bundled_version(),
            features: meta.features,
        })
    }
}

pub fn last_synced_at(store: &Store) -> Result<Option<i64>> {
    Ok(store
        .get_setting(LAST_SYNC_SETTING)?
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|&t| t > 0))
}

/// One configured connection to the sync endpoint, split so that no database
/// lock is ever held across a network call.
///
/// This split is not fussiness. The desktop app shares its `Store` behind a
/// mutex with the UI thread and the checker; a sync that held that lock for the
/// length of an HTTP round-trip would freeze the window for up to the request
/// timeout. So the phases are: read the store, talk to the network, write the
/// store, talk to the network — and [`drive`] sequences them without ever
/// owning the store itself.
pub struct SyncSession {
    client: SyncClient,
    cipher_key: [u8; 32],
}

impl SyncSession {
    /// Reads the connection out of the store. This is the only constructor step
    /// that touches it.
    pub fn begin(store: &Store) -> Result<Self> {
        let connection = Connection::load(store)?.context(
            "backup is not set up on this machine — connect a backend in Settings, \
             or run `litecter sync setup`",
        )?;
        Ok(Self {
            client: connection.client()?,
            cipher_key: connection.key.cipher_key(),
        })
    }

    /// Fetch and decrypt. Returns the empty document and no ETag when nothing
    /// has been stored yet.
    pub async fn pull(&self) -> Result<(SyncDoc, Option<String>)> {
        let Some(remote) = self.client.pull().await? else {
            return Ok((SyncDoc::default(), None));
        };
        let plain = crypto::open(&self.cipher_key, &remote.sealed)?;
        let parsed = serde_json::from_slice::<SyncDoc>(&plain)
            .context("the sync document could not be parsed")?;
        Ok((parsed, Some(remote.etag).filter(|e| !e.is_empty())))
    }

    /// Seal and conditionally replace. `Ok(None)` means another device won the
    /// race and the round should start again.
    async fn push(
        &self,
        doc: &SyncDoc,
        etag: Option<&str>,
    ) -> Result<Option<(String, usize)>> {
        let sealed = crypto::seal(&self.cipher_key, &serde_json::to_vec(doc)?)?;
        let size = sealed.len();
        Ok(self.client.push(&sealed, etag).await?.map(|tag| (tag, size)))
    }
}

/// Sequence one sync round, deferring every database touch to `store_step`.
///
/// `store_step` receives the remote document and must return the merged result
/// plus what applying it changed locally. Keeping it a closure is what lets the
/// CLI pass a plain `&Store` and the app pass a briefly-locked mutex guard,
/// without either re-implementing the retry loop.
///
/// On success returns the report and the new ETag; persist it with [`finish`].
pub async fn drive<F, Fut>(
    session: &SyncSession,
    mut store_step: F,
) -> Result<(SyncReport, String)>
where
    F: FnMut(SyncDoc) -> Fut,
    Fut: std::future::Future<Output = Result<(SyncDoc, ApplyStats)>>,
{
    let mut report = SyncReport::default();

    for attempt in 1..=MAX_ATTEMPTS {
        report.attempts = attempt;

        // The ETag always comes from this round's pull, never from a stored
        // one: a conditional push is only safe against the version actually
        // merged against.
        let (remote, etag) = session.pull().await?;
        let (mut merged, stats) = store_step(remote).await?;

        report.stats = stats;
        report.urls_in_document = merged.urls.len();
        report.diffs_dropped_for_size = merged.trim_to(doc::SOFT_BUDGET_BYTES)?;

        match session.push(&merged, etag.as_deref()).await? {
            Some((new_etag, size)) => {
                report.uploaded_bytes = size;
                return Ok((report, new_etag));
            }
            // Another device wrote between the pull and the push. Start over
            // against whatever it left behind — each attempt begins from
            // strictly newer state, so this converges rather than spins.
            None => continue,
        }
    }

    anyhow::bail!(
        "sync kept losing a race with another device after {MAX_ATTEMPTS} attempts; try again"
    )
}

/// Record a completed sync, clearing any outstanding failure.
pub fn finish(store: &Store, etag: &str, now: i64) -> Result<()> {
    store.set_setting(ETAG_SETTING, etag, now)?;
    store.set_setting(LAST_SYNC_SETTING, &now.to_string(), now)?;
    clear_failure(store, now)?;
    Ok(())
}

/// Whether the last attempt worked, and if not, since when and why.
///
/// This is persisted rather than kept in memory so a failure survives a restart.
/// A backup that quietly stopped working is worse than one that never started —
/// the user believes they are covered when they are not — so the failure has to
/// outlive the process that noticed it.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SyncHealth {
    /// When the current run of failures began. `None` when the last attempt
    /// succeeded.
    pub failing_since: Option<i64>,
    pub last_error: Option<String>,
}

impl SyncHealth {
    pub fn is_failing(&self) -> bool {
        self.failing_since.is_some()
    }
}

pub fn health(store: &Store) -> Result<SyncHealth> {
    Ok(SyncHealth {
        failing_since: store
            .get_setting(FAILING_SINCE_SETTING)?
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|&t| t > 0),
        last_error: store.get_setting(LAST_ERROR_SETTING)?.filter(|e| !e.is_empty()),
    })
}

/// Note a failed attempt. The *first* failure in a run sets `failing_since` and
/// later ones leave it alone, so the recorded age is how long the backup has
/// actually been broken rather than how long since the last retry.
pub fn record_failure(store: &Store, message: &str, now: i64) -> Result<()> {
    // "Cleared" is an empty value on an existing row, not a missing row, so
    // absence is the wrong test — checking `is_none()` here would mean the very
    // first success permanently prevented a failure from ever being recorded.
    let already_failing = health(store)?.is_failing();
    if !already_failing {
        store.set_setting(FAILING_SINCE_SETTING, &now.to_string(), now)?;
    }
    store.set_setting(LAST_ERROR_SETTING, message, now)?;
    Ok(())
}

pub fn clear_failure(store: &Store, now: i64) -> Result<()> {
    store.set_setting(FAILING_SINCE_SETTING, "", now)?;
    store.set_setting(LAST_ERROR_SETTING, "", now)?;
    store.set_setting(FAILURE_NOTIFIED_SETTING, "", now)?;
    Ok(())
}

/// Claim the right to tell the user the backup is broken.
///
/// Returns the message at most **once per run of failures**, and only once that
/// run has lasted [`FAILURE_NOTICE_AFTER_SECS`]. Recovering resets it, so a flaky
/// network produces at most one notification per genuine outage rather than a
/// stream of them — the same restraint the daily digest applies to changes.
pub fn take_failure_notice(store: &Store, now: i64) -> Result<Option<String>> {
    let health = health(store)?;
    let Some(since) = health.failing_since else {
        return Ok(None);
    };
    if now - since < FAILURE_NOTICE_AFTER_SECS {
        return Ok(None);
    }
    if store
        .get_setting(FAILURE_NOTIFIED_SETTING)?
        .is_some_and(|v| v == "1")
    {
        return Ok(None);
    }
    store.set_setting(FAILURE_NOTIFIED_SETTING, "1", now)?;
    Ok(Some(
        health.last_error.unwrap_or_else(|| "unknown error".into()),
    ))
}


/// The whole round against an exclusively-owned store — what the CLI wants.
/// Callers sharing a store behind a lock should use [`SyncSession`] and
/// [`drive`] directly so the lock is taken only inside the closure.
pub async fn sync_now(store: &Store, now: i64) -> Result<SyncReport> {
    let outcome = async {
        let session = SyncSession::begin(store)?;
        drive(&session, |remote| async move {
            let local = SyncDoc::build(store, now)?;
            let merged = doc::merge(local, remote);
            let stats = doc::apply(store, &merged, now)?;
            Ok((merged, stats))
        })
        .await
    }
    .await;

    // Record the outcome either way, so `litecter sync status` can report a
    // failure that happened on some earlier run — including one from the app.
    match outcome {
        Ok((report, etag)) => {
            finish(store, &etag, now)?;
            Ok(report)
        }
        Err(e) => {
            let _ = record_failure(store, &format!("{e:#}"), now);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3600;
    const DAY: i64 = 24 * HOUR;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    #[test]
    fn a_clean_store_is_healthy() {
        let s = store();
        assert!(!health(&s).unwrap().is_failing());
    }

    #[test]
    fn the_recorded_age_is_the_whole_outage_not_the_last_retry() {
        let s = store();
        record_failure(&s, "network unreachable", 1000).unwrap();
        record_failure(&s, "network unreachable", 1000 + HOUR).unwrap();
        record_failure(&s, "still unreachable", 1000 + 2 * HOUR).unwrap();

        let h = health(&s).unwrap();
        assert_eq!(h.failing_since, Some(1000), "must date from the first failure");
        assert_eq!(h.last_error.as_deref(), Some("still unreachable"), "newest message wins");
    }

    #[test]
    fn a_failure_after_a_success_is_still_recorded() {
        // The regression this guards: `clear_failure` blanks the value rather
        // than deleting the row, so testing for a *missing* setting made the
        // first success suppress every later failure.
        let s = store();
        record_failure(&s, "first outage", 1000).unwrap();
        clear_failure(&s, 2000).unwrap();
        assert!(!health(&s).unwrap().is_failing());

        record_failure(&s, "second outage", 3000).unwrap();
        let h = health(&s).unwrap();
        assert_eq!(h.failing_since, Some(3000));
        assert_eq!(h.last_error.as_deref(), Some("second outage"));
    }

    #[test]
    fn recovery_clears_the_alarm() {
        let s = store();
        record_failure(&s, "boom", 1000).unwrap();
        clear_failure(&s, 1000 + HOUR).unwrap();
        let h = health(&s).unwrap();
        assert_eq!(h.failing_since, None);
        assert_eq!(h.last_error, None);
    }

    #[test]
    fn the_notice_waits_a_day_then_fires_exactly_once() {
        let s = store();
        record_failure(&s, "network unreachable", 1000).unwrap();

        assert_eq!(take_failure_notice(&s, 1000 + HOUR).unwrap(), None, "too early to nag");
        assert_eq!(
            take_failure_notice(&s, 1000 + DAY).unwrap().as_deref(),
            Some("network unreachable"),
        );
        assert_eq!(
            take_failure_notice(&s, 1000 + 2 * DAY).unwrap(),
            None,
            "one notification per outage, not one per attempt",
        );
    }

    #[test]
    fn a_new_outage_earns_a_new_notice() {
        let s = store();
        record_failure(&s, "first", 1000).unwrap();
        assert!(take_failure_notice(&s, 1000 + DAY).unwrap().is_some());

        clear_failure(&s, 1000 + DAY + HOUR).unwrap();
        record_failure(&s, "second", 1000 + 2 * DAY).unwrap();
        assert_eq!(take_failure_notice(&s, 1000 + 2 * DAY + HOUR).unwrap(), None, "clock restarts");
        assert_eq!(
            take_failure_notice(&s, 1000 + 3 * DAY).unwrap().as_deref(),
            Some("second"),
        );
    }

    #[test]
    fn a_healthy_store_never_notifies() {
        let s = store();
        assert_eq!(take_failure_notice(&s, 1_000_000).unwrap(), None);
    }

    #[test]
    fn finishing_a_sync_clears_an_outstanding_failure() {
        let s = store();
        record_failure(&s, "boom", 1000).unwrap();
        finish(&s, "etag-123", 2000).unwrap();

        assert!(!health(&s).unwrap().is_failing());
        assert_eq!(last_synced_at(&s).unwrap(), Some(2000));
    }

    #[test]
    fn a_key_alone_is_not_a_working_backup() {
        // The state setup leaves behind between "here is your token" and "here
        // is where I deployed it". Calling it configured would put a banner on
        // screen and have the scheduler retrying against nothing.
        let s = store();
        assert!(!is_configured(&s).unwrap());
        ensure_key(&s, 1000).unwrap();
        assert!(!is_configured(&s).unwrap(), "a key with nowhere to send it is not set up");

        save_endpoint(&s, "https://litecter-sync.alice.workers.dev", 1000).unwrap();
        assert!(is_configured(&s).unwrap());
    }

    #[test]
    fn there_is_no_default_backend() {
        // Litecter runs no sync service. An endpoint we invented would be
        // either a server someone else pays for or an address that 404s.
        assert_eq!(endpoint(&store()).unwrap(), None);
    }

    #[test]
    fn changing_backends_drops_the_etag() {
        // The ETag describes a document on the old backend. Carrying it over
        // would make the first push to the new one fail a precondition about a
        // document that was never there.
        let s = store();
        save_endpoint(&s, "https://a.workers.dev", 1000).unwrap();
        finish(&s, "etag-from-a", 1000).unwrap();
        save_endpoint(&s, "https://b.workers.dev", 2000).unwrap();
        assert_eq!(s.get_setting(ETAG_SETTING).unwrap().as_deref(), Some(""));
    }

    #[test]
    fn re_saving_the_same_backend_keeps_the_etag() {
        let s = store();
        save_endpoint(&s, "https://a.workers.dev", 1000).unwrap();
        finish(&s, "etag-from-a", 1000).unwrap();
        save_endpoint(&s, "https://a.workers.dev/", 2000).unwrap();
        assert_eq!(s.get_setting(ETAG_SETTING).unwrap().as_deref(), Some("etag-from-a"));
    }

    #[test]
    fn a_connection_travels_to_another_machine_in_one_paste() {
        let a = store();
        ensure_key(&a, 1000).unwrap();
        save_endpoint(&a, "https://litecter-sync.alice.workers.dev", 1000).unwrap();

        let b = store();
        adopt_link(&b, &link_code(&a).unwrap().unwrap(), 2000).unwrap();

        assert!(is_configured(&b).unwrap());
        assert_eq!(endpoint(&b).unwrap(), endpoint(&a).unwrap());
        assert_eq!(
            load_key(&b).unwrap().unwrap().encode(),
            load_key(&a).unwrap().unwrap().encode(),
        );
    }

    #[test]
    fn disconnecting_forgets_the_connection_but_not_the_watch_list() {
        let s = store();
        ensure_key(&s, 1000).unwrap();
        save_endpoint(&s, "https://a.workers.dev", 1000).unwrap();
        record_failure(&s, "boom", 1000).unwrap();

        disconnect(&s, 2000).unwrap();
        assert!(!is_configured(&s).unwrap());
        assert_eq!(endpoint(&s).unwrap(), None);
        assert!(load_key(&s).unwrap().is_none());
        assert!(!health(&s).unwrap().is_failing(), "no stale alarm about a backup you stopped");
    }

    #[test]
    fn failure_state_is_never_synced_to_other_machines() {
        // These are machine-local: your laptop being offline says nothing about
        // your desktop's backup, and syncing them would make machines fight.
        for key in [KEY_SETTING, ETAG_SETTING, LAST_SYNC_SETTING, ENDPOINT_SETTING,
                    FAILING_SINCE_SETTING, LAST_ERROR_SETTING, FAILURE_NOTIFIED_SETTING] {
            assert!(!doc::SYNCED_SETTINGS.contains(&key), "{key} must stay local");
        }
    }
}
