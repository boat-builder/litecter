#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Timelike;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_notification::NotificationExt as _;
use tokio::sync::Mutex;

use litecter_core::{
    differ, fetch_rendered, persist_check, sync, ChangeItem, Renderer, Schedule, Store, UrlRow,
};

type SharedStore = Arc<Mutex<Store>>;

struct AppState {
    store: SharedStore,
    sync: SyncScheduler,
    worker: WorkerState,
}

/// Coordinates when a sync runs.
///
/// Two triggers, for two different failure modes. The daily tick is the backup
/// guarantee. The debounced push is what keeps the window of loss small: adding
/// thirty URLs and losing the disk an hour later shouldn't cost you all thirty,
/// and a 60-second debounce collapses that whole burst into one upload.
#[derive(Clone)]
struct SyncScheduler {
    state: Arc<Mutex<SyncState>>,
    running: Arc<Mutex<()>>,
}

#[derive(Default)]
struct SyncState {
    /// When the watch list first changed since the last successful sync.
    dirty_since: Option<i64>,
    /// Don't attempt again before this — exponential backoff after a failure,
    /// so a laptop that is simply offline doesn't retry every minute all day.
    not_before: i64,
    failures: u32,
}

/// A burst of edits collapses into one upload this long after the first of them.
const SYNC_DEBOUNCE_SECS: i64 = 60;
/// The floor: sync at least this often even if nothing changed locally, so a
/// second machine's edits still arrive.
const SYNC_INTERVAL_SECS: i64 = 24 * 60 * 60;
/// Backoff after a failed attempt: 5 min × 2^failures, capped at an hour.
const SYNC_BACKOFF_BASE_SECS: i64 = 300;
const SYNC_BACKOFF_CAP_SECS: i64 = 3600;

impl SyncScheduler {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(SyncState::default())),
            running: Arc::new(Mutex::new(())),
        }
    }

    /// Note that the watch list changed. Only the *first* edit in a burst sets
    /// the timer, so a bulk import doesn't keep pushing its own deadline back.
    async fn mark_dirty(&self, now: i64) {
        let mut state = self.state.lock().await;
        if state.dirty_since.is_none() {
            state.dirty_since = Some(now);
        }
    }

    /// Whether to sync now. Clears the debounce flag, so a caller that takes a
    /// `true` must report the outcome back via [`succeeded`](Self::succeeded)
    /// or [`failed`](Self::failed) — otherwise a pending edit is dropped.
    async fn take_if_due(&self, now: i64, last_sync: Option<i64>) -> bool {
        let mut state = self.state.lock().await;
        if now < state.not_before {
            return false;
        }
        let debounced = state.dirty_since.is_some_and(|since| now - since >= SYNC_DEBOUNCE_SECS);
        let overdue = last_sync.is_none_or(|t| now - t >= SYNC_INTERVAL_SECS);
        if debounced || overdue {
            state.dirty_since = None;
            return true;
        }
        false
    }

    async fn succeeded(&self) {
        let mut state = self.state.lock().await;
        state.failures = 0;
        state.not_before = 0;
    }

    /// Re-arm the edit that just failed to upload. Without this, one transient
    /// network error would silently postpone a backup until the next daily
    /// tick — exactly the window this whole feature exists to close.
    async fn failed(&self, now: i64) {
        let mut state = self.state.lock().await;
        state.failures = state.failures.saturating_add(1);
        let delay = SYNC_BACKOFF_BASE_SECS
            .saturating_mul(1i64 << state.failures.min(4))
            .min(SYNC_BACKOFF_CAP_SECS);
        state.not_before = now + delay;
        if state.dirty_since.is_none() {
            state.dirty_since = Some(now);
        }
    }
}

/// Run one sync round.
///
/// The store lock is taken three times, each for a short synchronous stretch,
/// and never across an HTTP call — otherwise the window would freeze for the
/// length of the request. `sync::drive` sequences the network; this closure
/// owns all the database work.
async fn sync_once(store: &SharedStore, scheduler: &SyncScheduler) -> Result<sync::SyncReport, String> {
    // A slow sync must not stack up behind the next tick. Contention is not a
    // failure, so it returns before anything is recorded against the backup's
    // health — otherwise pressing "Sync now" mid-tick would look like an outage.
    let _guard = scheduler.running.try_lock().map_err(|_| "a sync is already running".to_string())?;
    let now = now_ts();

    let outcome = async {
        let session = {
            let s = store.lock().await;
            sync::SyncSession::begin(&s)?
        };
        sync::drive(&session, |remote| async move {
            let s = store.lock().await;
            let local = sync::SyncDoc::build(&s, now)?;
            let merged = litecter_core::sync::doc::merge(local, remote);
            let stats = litecter_core::sync::doc::apply(&s, &merged, now)?;
            Ok((merged, stats))
        })
        .await
    }
    .await;

    let s = store.lock().await;
    match outcome {
        Ok((report, etag)) => {
            let _ = sync::finish(&s, &etag, now);
            Ok(report)
        }
        Err(e) => {
            let message = format!("{e:#}");
            let _ = sync::record_failure(&s, &message, now);
            Err(message)
        }
    }
}

/// The background trigger: daily, or 60 s after the watch list last changed.
/// Failures are logged and dropped — a backup that can't reach the network must
/// never interrupt checking or take the app down.
async fn maybe_sync(app: &AppHandle, store: &SharedStore, scheduler: &SyncScheduler) {
    let (configured, last) = {
        let s = store.lock().await;
        (
            sync::is_configured(&s).unwrap_or(false),
            sync::last_synced_at(&s).ok().flatten(),
        )
    };
    if !configured || !scheduler.take_if_due(now_ts(), last).await {
        return;
    }
    match sync_once(store, scheduler).await {
        Ok(report) => {
            scheduler.succeeded().await;
            if report.stats.added > 0 || report.stats.removed > 0 || report.stats.pendings_restored > 0
            {
                update_tray(app, store).await;
            }
            // Always refresh: the health banner has to clear itself once the
            // backup recovers, not just when the watch list moved.
            let _ = app.emit("litecter://refresh", ());
        }
        Err(e) => {
            scheduler.failed(now_ts()).await;
            eprintln!("sync failed (will retry): {e}");
            notify_if_backup_is_stuck(app, store).await;
            let _ = app.emit("litecter://refresh", ());
        }
    }
}

/// The last line of defence: a user who never opens the window would otherwise
/// never learn their backup stopped working.
///
/// Fires at most **once per outage**, and only after a full day of failure —
/// the same restraint the daily digest applies to changes. See the note in
/// `litecter_core::scheduler::tick` before adding any other notification.
async fn notify_if_backup_is_stuck(app: &AppHandle, store: &SharedStore) {
    let notice = {
        let s = store.lock().await;
        sync::take_failure_notice(&s, now_ts()).ok().flatten()
    };
    let Some(reason) = notice else { return };
    let _ = app
        .notification()
        .builder()
        .title("Litecter backup is failing")
        .body(format!(
            "Your watch list hasn't backed up in over a day — {reason}"
        ))
        .show();
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before 1970")
        .as_secs() as i64
}

fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("LITECTER_DB") {
        return p.into();
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("litecter")
        .join("litecter.db")
}

fn normalize_input_url(raw: &str) -> String {
    let raw = raw.trim();
    if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    }
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Show the window with the add bar focused — the tray's "Add link" path.
/// Emitted after `show_main` so the webview is visible before it takes focus.
fn show_add(app: &AppHandle) {
    show_main(app);
    let _ = app.emit("litecter://focus-add", ());
}

// ---- background checking ----------------------------------------------------

/// Check a batch with the wake → check → sleep browser lifecycle. The store
/// lock is held only for the fast persist step, never across a render.
async fn run_batch(app: &AppHandle, store: &SharedStore, list: Vec<UrlRow>) {
    let renderer = match Renderer::launch().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("browser launch failed: {e:#}");
            return;
        }
    };
    let mut last_hit: HashMap<String, tokio::time::Instant> = HashMap::new();
    const PER_HOST_GAP: Duration = Duration::from_secs(10);
    for u in &list {
        if let Some(host) = url::Url::parse(&u.url)
            .ok()
            .and_then(|p| p.host_str().map(str::to_string))
        {
            if let Some(prev) = last_hit.get(&host) {
                let elapsed = prev.elapsed();
                if elapsed < PER_HOST_GAP {
                    tokio::time::sleep(PER_HOST_GAP - elapsed).await;
                }
            }
            last_hit.insert(host, tokio::time::Instant::now());
        }
        let fetched = fetch_rendered(&renderer, u).await;
        {
            let s = store.lock().await;
            persist_check(&s, u, fetched, now_ts());
        }
        let _ = app.emit("litecter://refresh", ());
    }
    renderer.shutdown().await;
    update_tray(app, store).await;
}

async fn spawn_check(app: AppHandle, store: SharedStore, ids: Option<Vec<i64>>) {
    let list: Vec<UrlRow> = {
        let s = store.lock().await;
        match ids {
            Some(ids) => ids
                .iter()
                .filter_map(|id| s.url_by_id(*id).ok().flatten())
                .collect(),
            None => s.due_urls(now_ts()).unwrap_or_default(),
        }
    };
    if !list.is_empty() {
        run_batch(&app, &store, list).await;
    }
    let _ = app.emit("litecter://refresh", ());
}

async fn update_tray(app: &AppHandle, store: &SharedStore) {
    let unseen = { store.lock().await.count_unseen().unwrap_or(0) };
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_title(if unseen > 0 {
            Some(unseen.to_string())
        } else {
            None::<String>
        });
    }
}

/// Once per local day at/after the configured hour, while unseen changes
/// exist — the re-nag until inbox zero.
///
/// The digest is the only notification the app sends; a detected change updates
/// the tray badge and nothing else. See the matching note in
/// `litecter_core::scheduler::tick` before adding per-change pings.
async fn maybe_digest(app: &AppHandle, store: &SharedStore) {
    let local = chrono::Local::now();
    let digest_hour: u32 = {
        store
            .lock()
            .await
            .get_setting("digest_hour")
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(9)
    };
    if local.hour() < digest_hour {
        return;
    }
    let today = local.format("%Y-%m-%d").to_string();
    let (due_today, unseen) = {
        let s = store.lock().await;
        let last = s.get_setting("last_digest_date").ok().flatten();
        (last.as_deref() != Some(today.as_str()), s.count_unseen().unwrap_or(0))
    };
    if !due_today {
        return;
    }
    if unseen > 0 {
        let _ = app
            .notification()
            .builder()
            .title("Litecter")
            .body(format!("{unseen} page(s) have unreviewed changes"))
            .show();
    }
    let _ = store.lock().await.set_setting("last_digest_date", &today, now_ts());
}

async fn scheduler_loop(app: AppHandle, store: SharedStore, scheduler: SyncScheduler) {
    loop {
        let due = { store.lock().await.due_urls(now_ts()).unwrap_or_default() };
        if !due.is_empty() {
            run_batch(&app, &store, due).await;
        }
        maybe_digest(&app, &store).await;
        maybe_sync(&app, &store, &scheduler).await;
        update_tray(&app, &store).await;
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

// ---- commands ----------------------------------------------------------------

#[tauri::command]
async fn list_urls(state: State<'_, AppState>) -> Result<Vec<UrlRow>, String> {
    state.store.lock().await.list_urls().map_err(|e| e.to_string())
}

/// Returns per-URL error strings for anything that couldn't be added.
#[tauri::command]
async fn add_urls(
    state: State<'_, AppState>,
    urls: Vec<String>,
    every: String,
) -> Result<Vec<String>, String> {
    let schedule: Schedule = every.parse().map_err(|e: anyhow::Error| e.to_string())?;
    let now = now_ts();
    let mut errors = Vec::new();
    {
        let s = state.store.lock().await;
        for raw in urls {
            let url = normalize_input_url(&raw);
            if let Err(e) = s.add_url(&url, schedule, None, now) {
                errors.push(format!("{url}: {e:#}"));
            }
        }
    }
    state.sync.mark_dirty(now).await;
    Ok(errors)
}

#[tauri::command]
async fn remove_urls(state: State<'_, AppState>, ids: Vec<i64>) -> Result<(), String> {
    let now = now_ts();
    {
        let s = state.store.lock().await;
        for id in ids {
            s.remove_url(id, now).map_err(|e| e.to_string())?;
        }
    }
    state.sync.mark_dirty(now).await;
    Ok(())
}

#[tauri::command]
async fn set_schedule(
    state: State<'_, AppState>,
    ids: Vec<i64>,
    every: String,
) -> Result<(), String> {
    let schedule: Schedule = every.parse().map_err(|e: anyhow::Error| e.to_string())?;
    let now = now_ts();
    {
        let s = state.store.lock().await;
        for id in ids {
            s.set_schedule(id, schedule, now).map_err(|e| e.to_string())?;
        }
    }
    state.sync.mark_dirty(now).await;
    Ok(())
}

#[tauri::command]
async fn check_now(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Option<Vec<i64>>,
) -> Result<(), String> {
    let store = state.store.clone();
    tauri::async_runtime::spawn(spawn_check(app, store, ids));
    Ok(())
}

#[tauri::command]
async fn list_changes(
    state: State<'_, AppState>,
    unseen_only: bool,
) -> Result<Vec<ChangeItem>, String> {
    state
        .store
        .lock()
        .await
        .list_changes(unseen_only)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_diff(state: State<'_, AppState>, change_id: i64) -> Result<String, String> {
    let s = state.store.lock().await;
    let c = s
        .change_by_id(change_id)
        .map_err(|e| e.to_string())?
        .ok_or("change not found")?;
    let from = s.snapshot_text(c.from_snapshot_id).map_err(|e| e.to_string())?;
    let to = s.snapshot_text(c.to_snapshot_id).map_err(|e| e.to_string())?;
    Ok(differ::unified(&from, &to, 3))
}

#[tauri::command]
async fn mark_seen(
    app: AppHandle,
    state: State<'_, AppState>,
    change_ids: Vec<i64>,
) -> Result<(), String> {
    {
        let s = state.store.lock().await;
        let now = now_ts();
        for id in change_ids {
            s.mark_seen(id, now).map_err(|e| e.to_string())?;
        }
    }
    update_tray(&app, &state.store).await;
    Ok(())
}

#[tauri::command]
async fn mark_all_seen(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    {
        let s = state.store.lock().await;
        s.mark_all_seen(now_ts()).map_err(|e| e.to_string())?;
    }
    update_tray(&app, &state.store).await;
    Ok(())
}

// ---- sync commands -------------------------------------------------------------

#[derive(serde::Serialize)]
struct SyncStatus {
    configured: bool,
    /// Only ever sent to the local webview, and only to be displayed once so
    /// the user can save it. It is not shown unless they ask.
    key: Option<String>,
    /// The whole connection as one paste, for a second machine.
    link: Option<String>,
    /// What the backend's `SYNC_TOKEN` secret has to be set to.
    token: Option<String>,
    last_synced_at: Option<i64>,
    watched: usize,
    endpoint: Option<String>,
    /// Set while the backup is broken — drives the banner in the main window.
    failing_since: Option<i64>,
    last_error: Option<String>,
    /// The worker version this build ships. `None` means the constant could not
    /// be read, which disables the update nag rather than inventing a number.
    bundled_worker_version: Option<u32>,
    /// What the last probe found. `None` while unknown — see [`WorkerState`].
    deployed_worker_version: Option<u32>,
    worker_outdated: bool,
    worker_needs_token_secret: bool,
}

/// The last answer the backend gave about its own version.
///
/// Cached because it is a network call and the UI polls status every 30
/// seconds. `None` means *unknown*, which is deliberately not the same as
/// outdated: a laptop on a plane must not be told to redeploy a worker that was
/// already current.
#[derive(Clone, Default)]
struct WorkerState(Arc<Mutex<Option<sync::WorkerCheck>>>);

#[tauri::command]
async fn get_sync_status(state: State<'_, AppState>) -> Result<SyncStatus, String> {
    let worker = state.worker.0.lock().await.clone();
    let s = state.store.lock().await;
    let health = sync::health(&s).map_err(|e| e.to_string())?;
    let key = sync::load_key(&s).map_err(|e| e.to_string())?;
    Ok(SyncStatus {
        configured: sync::is_configured(&s).map_err(|e| e.to_string())?,
        key: key.as_ref().map(|k| k.encode()),
        link: sync::link_code(&s).map_err(|e| e.to_string())?,
        token: key.as_ref().map(|k| k.auth_token()),
        last_synced_at: sync::last_synced_at(&s).map_err(|e| e.to_string())?,
        watched: s.list_urls().map_err(|e| e.to_string())?.len(),
        endpoint: sync::endpoint(&s).map_err(|e| e.to_string())?,
        failing_since: health.failing_since,
        last_error: health.last_error,
        bundled_worker_version: sync::worker::bundled_version(),
        deployed_worker_version: worker.as_ref().map(|w| w.deployed),
        worker_outdated: worker.as_ref().is_some_and(|w| w.is_outdated()),
        worker_needs_token_secret: worker.as_ref().is_some_and(|w| w.needs_token_secret()),
    })
}

/// The backend's source, for the "copy worker code" button. The same bytes the
/// release attaches and the same bytes we parse the version out of.
#[tauri::command]
fn get_worker_source() -> &'static str {
    sync::worker::SOURCE
}

#[derive(serde::Serialize)]
struct Instructions {
    text: String,
    /// Shown at the copy button. Setup's agent and terminal routes carry the
    /// token; nothing in the update flow does, because a same-name redeploy
    /// leaves the secret untouched.
    carries_secret: bool,
}

/// Generate one route's hand-off. `kind` is "setup" or "update".
#[tauri::command]
async fn get_instructions(
    state: State<'_, AppState>,
    kind: String,
    route: String,
) -> Result<Instructions, String> {
    let route = sync::setup::Route::parse(&route).ok_or("unknown route")?;
    let updating = kind == "update";

    let (token, endpoint, needs_secret) = {
        let s = state.store.lock().await;
        // Setup needs the token before the backend exists, so this is where the
        // key is born rather than on the first sync.
        let key = if updating {
            sync::load_key(&s).map_err(|e| e.to_string())?
        } else {
            Some(sync::ensure_key(&s, now_ts()).map_err(|e| e.to_string())?)
        };
        (
            key.map(|k| k.auth_token()).unwrap_or_default(),
            sync::endpoint(&s).map_err(|e| e.to_string())?,
            state.worker.0.lock().await.as_ref().is_some_and(|w| w.needs_token_secret()),
        )
    };

    let text = if updating {
        let deployment = sync::setup::Deployment::from_endpoint(&endpoint.unwrap_or_default());
        sync::setup::update(route, &deployment, needs_secret)
    } else {
        sync::setup::setup(route, &token)
    };
    Ok(Instructions { text, carries_secret: !updating && route.setup_carries_secret() })
}

/// Point this machine at a backend, after proving it answers.
///
/// Verification is the whole reason this is a round trip rather than a save:
/// Litecter cannot see the user's Cloudflare account, so checking from this side
/// is the only confirmation that means anything. "The instructions said it
/// worked" is not evidence.
///
/// `restoring` says which half of setup is calling, and only changes what a
/// rejected token says — see [`sync::Intent`]. It is passed rather than inferred
/// because both paths end at the same call with the same arguments; only the
/// user's intent differs, and only they know it.
#[tauri::command]
async fn connect_backend(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
    restoring: bool,
) -> Result<(), String> {
    let endpoint = sync::link::normalize_endpoint(&url).map_err(|e| format!("{e:#}"))?;
    let connection = {
        let s = state.store.lock().await;
        sync::Connection {
            endpoint: endpoint.clone(),
            key: sync::ensure_key(&s, now_ts()).map_err(|e| e.to_string())?,
        }
    };

    let intent = if restoring { sync::Intent::Adopting } else { sync::Intent::Connecting };
    connection.verify(intent).await.map_err(|e| format!("{e:#}"))?;

    {
        let s = state.store.lock().await;
        sync::save_endpoint(&s, &endpoint, now_ts()).map_err(|e| e.to_string())?;
    }
    refresh_worker_check(&state.store, &state.worker).await;
    let _ = app.emit("litecter://refresh", ());
    Ok(())
}

/// Adopt a connection from another machine — the combined paste, or a bare key.
///
/// Returns whether the paste carried a backend address as well as a key. A bare
/// key is what earlier versions handed out, so someone will paste one: it leaves
/// the machine holding half a connection, and the caller has to go and ask for
/// the other half rather than report success.
#[tauri::command]
async fn adopt_link(
    app: AppHandle,
    state: State<'_, AppState>,
    code: String,
) -> Result<bool, String> {
    let parsed = sync::link::parse(&code).map_err(|e| format!("{e:#}"))?;

    // Check before saving when the paste carried an address, so a typo is
    // rejected here rather than surfacing as a broken backup tomorrow.
    if let Some(endpoint) = &parsed.endpoint {
        sync::Connection { endpoint: endpoint.clone(), key: parsed.key }
            .verify(sync::Intent::Adopting)
            .await
            .map_err(|e| format!("{e:#}"))?;
    }

    {
        let s = state.store.lock().await;
        sync::adopt_link(&s, &code, now_ts()).map_err(|e| format!("{e:#}"))?;
    }
    refresh_worker_check(&state.store, &state.worker).await;
    let _ = app.emit("litecter://refresh", ());

    let s = state.store.lock().await;
    sync::is_configured(&s).map_err(|e| e.to_string())
}

/// Re-probe the backend's version. This is what "Check again" calls.
#[tauri::command]
async fn check_worker(state: State<'_, AppState>) -> Result<Option<u32>, String> {
    let connection = {
        let s = state.store.lock().await;
        sync::Connection::load(&s).map_err(|e| e.to_string())?
    };
    let Some(connection) = connection else { return Ok(None) };
    let check = connection.probe().await.map_err(|e| format!("{e:#}"))?;
    let deployed = check.deployed;
    *state.worker.0.lock().await = Some(check);
    Ok(Some(deployed))
}

/// Erase the backup from the user's own bucket.
///
/// Step one of removing a backend: R2 will not delete a bucket that still holds
/// objects, and nothing but this app holds the token that can reach this one.
#[tauri::command]
async fn erase_backup(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let connection = {
        let s = state.store.lock().await;
        sync::Connection::load(&s).map_err(|e| e.to_string())?
    };
    connection
        .ok_or("backup is not set up on this machine")?
        .erase()
        .await
        .map_err(|e| format!("{e:#}"))?;

    // The stored ETag describes a document that no longer exists; keeping it
    // would make the next push fail a precondition about nothing.
    {
        let s = state.store.lock().await;
        sync::disconnect(&s, now_ts()).map_err(|e| e.to_string())?;
    }
    *state.worker.0.lock().await = None;
    let _ = app.emit("litecter://refresh", ());
    Ok(())
}

/// Stop syncing without touching the backup or the watch list.
#[tauri::command]
async fn disconnect_backend(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    {
        let s = state.store.lock().await;
        sync::disconnect(&s, now_ts()).map_err(|e| e.to_string())?;
    }
    *state.worker.0.lock().await = None;
    let _ = app.emit("litecter://refresh", ());
    Ok(())
}

/// Probe on launch and after a connection changes. Failures are swallowed on
/// purpose — an unreachable backend leaves the version unknown, and unknown
/// must never read as outdated.
async fn refresh_worker_check(store: &SharedStore, worker: &WorkerState) {
    let connection = {
        let s = store.lock().await;
        sync::Connection::load(&s).ok().flatten()
    };
    let Some(connection) = connection else {
        *worker.0.lock().await = None;
        return;
    };
    if let Ok(check) = connection.probe().await {
        *worker.0.lock().await = Some(check);
    }
}

#[derive(serde::Serialize)]
struct SyncOutcome {
    urls: usize,
    added: usize,
    removed: usize,
    pendings_restored: usize,
    uploaded_bytes: usize,
    diffs_dropped_for_size: usize,
}

#[tauri::command]
async fn sync_now_cmd(app: AppHandle, state: State<'_, AppState>) -> Result<SyncOutcome, String> {
    // A manual sync deliberately ignores the backoff window — the user asking
    // is better evidence that the network is back than any timer.
    let report = sync_once(&state.store, &state.sync).await?;
    state.sync.succeeded().await;
    let _ = app.emit("litecter://refresh", ());
    update_tray(&app, &state.store).await;
    Ok(SyncOutcome {
        urls: report.urls_in_document,
        added: report.stats.added,
        removed: report.stats.removed,
        pendings_restored: report.stats.pendings_restored,
        uploaded_bytes: report.uploaded_bytes,
        diffs_dropped_for_size: report.diffs_dropped_for_size,
    })
}

#[derive(serde::Serialize)]
struct Prefs {
    digest_hour: u32,
    autostart: bool,
}

#[tauri::command]
async fn get_prefs(app: AppHandle, state: State<'_, AppState>) -> Result<Prefs, String> {
    let digest_hour = state
        .store
        .lock()
        .await
        .get_setting("digest_hour")
        .map_err(|e| e.to_string())?
        .and_then(|v| v.parse().ok())
        .unwrap_or(9);
    let autostart = app.autolaunch().is_enabled().unwrap_or(false);
    Ok(Prefs { digest_hour, autostart })
}

#[tauri::command]
async fn set_prefs(
    app: AppHandle,
    state: State<'_, AppState>,
    digest_hour: u32,
    autostart: bool,
) -> Result<(), String> {
    if digest_hour > 23 {
        return Err("digest hour must be 0-23".into());
    }
    state
        .store
        .lock()
        .await
        .set_setting("digest_hour", &digest_hour.to_string(), now_ts())
        .map_err(|e| e.to_string())?;
    let launcher = app.autolaunch();
    if autostart {
        launcher.enable().map_err(|e| e.to_string())?;
    } else {
        launcher.disable().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Opens an http(s) URL in the default browser — the updater's "Download
/// manually…" escape hatch. macOS-only, like the rest of the app; a
/// cross-platform port would use xdg-open / `start`.
#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(format!("refusing to open non-http url: {url}"));
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&url)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("open {url}: {e}"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("open_external is only supported on macOS".to_string())
    }
}

// ---- app ----------------------------------------------------------------------

fn main() {
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_notification::init());

    // In-app auto-update. The updater plugin has no mobile implementation, so
    // it is desktop-gated here to match the target block in Cargo.toml; the
    // process plugin is what relaunches the app once the bundle is swapped —
    // without it the update installs but the user stays on the old version
    // until they quit by hand. See docs/auto-update.md.
    #[cfg(desktop)]
    {
        builder = builder
            .plugin(tauri_plugin_updater::Builder::new().build())
            .plugin(tauri_plugin_process::init());
    }

    builder
        .setup(|app| {
            let store = Store::open(&db_path())?;
            // First run: launch-at-login on by default (user can toggle it off).
            if store.get_setting("autostart_initialized")?.is_none() {
                let _ = app.autolaunch().enable();
                store.set_setting("autostart_initialized", "1", now_ts())?;
            }
            let store: SharedStore = Arc::new(Mutex::new(store));
            let sync_scheduler = SyncScheduler::new();
            let worker_state = WorkerState::default();
            app.manage(AppState {
                store: store.clone(),
                sync: sync_scheduler.clone(),
                worker: worker_state.clone(),
            });

            let open = MenuItem::with_id(app, "open", "Open Litecter", true, None::<&str>)?;
            let add = MenuItem::with_id(app, "add", "Add link…", true, None::<&str>)?;
            let check = MenuItem::with_id(app, "check", "Check due now", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Litecter", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &add, &check, &quit])?;
            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().expect("bundled icon").clone())
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main(app),
                    "add" => show_add(app),
                    "check" => {
                        let state: State<'_, AppState> = app.state();
                        let store = state.store.clone();
                        tauri::async_runtime::spawn(spawn_check(app.clone(), store, None));
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            // Hidden at login (--hidden via autostart); visible on manual launch.
            if !std::env::args().any(|a| a == "--hidden") {
                show_main(app.handle());
            }

            tauri::async_runtime::spawn(scheduler_loop(
                app.handle().clone(),
                store.clone(),
                sync_scheduler,
            ));

            // One probe per launch, off the startup path. Nothing waits on it:
            // the window opens and works whether or not the backend answers.
            tauri::async_runtime::spawn(async move {
                refresh_worker_check(&store, &worker_state).await;
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_urls,
            add_urls,
            remove_urls,
            set_schedule,
            check_now,
            list_changes,
            get_diff,
            mark_seen,
            mark_all_seen,
            get_prefs,
            set_prefs,
            get_sync_status,
            get_worker_source,
            get_instructions,
            connect_backend,
            adopt_link,
            check_worker,
            erase_backup,
            disconnect_backend,
            sync_now_cmd,
            open_external
        ])
        .run(tauri::generate_context!())
        .expect("error while running Litecter");
}
