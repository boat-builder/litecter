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
    differ, fetch_rendered, persist_check, ChangeItem, Renderer, Schedule, Store, UrlRow,
};

type SharedStore = Arc<Mutex<Store>>;

struct AppState {
    store: SharedStore,
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
    let _ = store.lock().await.set_setting("last_digest_date", &today);
}

async fn scheduler_loop(app: AppHandle, store: SharedStore) {
    loop {
        let due = { store.lock().await.due_urls(now_ts()).unwrap_or_default() };
        if !due.is_empty() {
            run_batch(&app, &store, due).await;
        }
        maybe_digest(&app, &store).await;
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
    let s = state.store.lock().await;
    let now = now_ts();
    let mut errors = Vec::new();
    for raw in urls {
        let url = normalize_input_url(&raw);
        if let Err(e) = s.add_url(&url, schedule, None, now) {
            errors.push(format!("{url}: {e:#}"));
        }
    }
    Ok(errors)
}

#[tauri::command]
async fn remove_urls(state: State<'_, AppState>, ids: Vec<i64>) -> Result<(), String> {
    let s = state.store.lock().await;
    for id in ids {
        s.remove_url(id).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn set_schedule(
    state: State<'_, AppState>,
    ids: Vec<i64>,
    every: String,
) -> Result<(), String> {
    let schedule: Schedule = every.parse().map_err(|e: anyhow::Error| e.to_string())?;
    let s = state.store.lock().await;
    let now = now_ts();
    for id in ids {
        s.set_schedule(id, schedule, now).map_err(|e| e.to_string())?;
    }
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
        .set_setting("digest_hour", &digest_hour.to_string())
        .map_err(|e| e.to_string())?;
    let launcher = app.autolaunch();
    if autostart {
        launcher.enable().map_err(|e| e.to_string())?;
    } else {
        launcher.disable().map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ---- app ----------------------------------------------------------------------

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let store = Store::open(&db_path())?;
            // First run: launch-at-login on by default (user can toggle it off).
            if store.get_setting("autostart_initialized")?.is_none() {
                let _ = app.autolaunch().enable();
                store.set_setting("autostart_initialized", "1")?;
            }
            let store: SharedStore = Arc::new(Mutex::new(store));
            app.manage(AppState { store: store.clone() });

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

            tauri::async_runtime::spawn(scheduler_loop(app.handle().clone(), store));
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
            set_prefs
        ])
        .run(tauri::generate_context!())
        .expect("error while running Litecter");
}
