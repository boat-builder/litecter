use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use chrono::{Local, Timelike};
use tokio::time::{sleep, Instant};

use crate::checker::{check_one, CheckResult};
use crate::renderer::Renderer;
use crate::store::Store;

const TICK: Duration = Duration::from_secs(60);
const PER_HOST_GAP: Duration = Duration::from_secs(10);

pub struct DaemonOptions {
    /// Local hour (0–23) at which the unseen-changes digest fires.
    pub digest_hour: u32,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self { digest_hour: 9 }
    }
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before 1970")
        .as_secs() as i64
}

fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(str::to_string)
}

/// The long-running loop: every minute, check whatever is due (wake → check →
/// sleep: the browser exists only while a batch drains), then fire the daily
/// digest if it's time. Returns on Ctrl-C.
pub async fn run_daemon<N>(store: &Store, opts: DaemonOptions, notify: N) -> Result<()>
where
    N: Fn(&str, &str),
{
    eprintln!(
        "litecter daemon: tick every {}s, digest at {:02}:00 local",
        TICK.as_secs(),
        opts.digest_hour
    );
    loop {
        if let Err(e) = tick(store, &opts, &notify).await {
            eprintln!("tick failed: {e:#}");
        }
        tokio::select! {
            _ = sleep(TICK) => {}
            _ = tokio::signal::ctrl_c() => {
                eprintln!("litecter daemon: shutting down");
                return Ok(());
            }
        }
    }
}

async fn tick<N: Fn(&str, &str)>(store: &Store, opts: &DaemonOptions, notify: &N) -> Result<()> {
    let due = store.due_urls(now_ts())?;
    if !due.is_empty() {
        eprintln!("checking {} due URL(s)…", due.len());
        match Renderer::launch().await {
            Ok(renderer) => {
                let mut last_hit: HashMap<String, Instant> = HashMap::new();
                for u in &due {
                    if let Some(host) = host_of(&u.url) {
                        if let Some(prev) = last_hit.get(&host) {
                            let elapsed = prev.elapsed();
                            if elapsed < PER_HOST_GAP {
                                sleep(PER_HOST_GAP - elapsed).await;
                            }
                        }
                        last_hit.insert(host, Instant::now());
                    }
                    let label = u.title.as_deref().unwrap_or(&u.url);
                    match check_one(store, &renderer, u, now_ts()).await {
                        CheckResult::Changed { added, removed } => {
                            eprintln!("  * {label} — changed +{added} −{removed}")
                        }
                        CheckResult::Errored(msg) => eprintln!("  ! {label} — {msg}"),
                        _ => {}
                    }
                }
                renderer.shutdown().await;
            }
            Err(e) => eprintln!("browser launch failed, will retry next tick: {e:#}"),
        }
    }

    // Daily digest: once per local day, at or after the configured hour,
    // and only while unseen changes exist — the re-nag until inbox-zero.
    //
    // This is the *only* notification Litecter sends. Detecting a change
    // deliberately does not notify: a batch that finds 20 changes would mean 20
    // interruptions, and the whole point of the digest is that missing one
    // notification costs nothing because tomorrow's repeats it. Don't add
    // per-change pings here without making them opt-in.
    let local = Local::now();
    if local.hour() >= opts.digest_hour {
        let today = local.format("%Y-%m-%d").to_string();
        if store.get_setting("last_digest_date")?.as_deref() != Some(today.as_str()) {
            let unseen = store.count_unseen()?;
            if unseen > 0 {
                notify(
                    "Litecter",
                    &format!("{unseen} page(s) have unreviewed changes — litecter changes"),
                );
            }
            store.set_setting("last_digest_date", &today, now_ts())?;
        }
    }
    Ok(())
}
