use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use litecter_core::{
    check_one, differ, run_daemon, sync, CheckResult, DaemonOptions, Renderer, Schedule, Store,
    UrlRow,
};

#[derive(Parser)]
#[command(
    name = "litecter",
    version,
    about = "Watch web pages for changes — rendered in a real browser."
)]
struct Cli {
    /// Database path (default: platform data dir)
    #[arg(long, global = true, env = "LITECTER_DB")]
    db: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Add one or more URLs to watch
    Add {
        urls: Vec<String>,
        /// hourly | daily | weekly | monthly
        #[arg(long, default_value = "weekly")]
        every: String,
        /// Watch only the part of the page matching this CSS selector
        #[arg(long)]
        selector: Option<String>,
        /// Read URLs (one per line, # comments) from a file
        #[arg(long)]
        from_file: Option<PathBuf>,
    },
    /// List watched URLs
    List {
        #[arg(long)]
        json: bool,
    },
    /// Stop watching URLs (by id or url)
    Rm { targets: Vec<String> },
    /// Run checks now (only due URLs by default)
    Check {
        /// Check everything regardless of schedule
        #[arg(long)]
        all: bool,
        /// Specific ids/urls to check
        targets: Vec<String>,
    },
    /// List detected changes (unseen first)
    Changes {
        /// Only unreviewed changes
        #[arg(long)]
        unseen: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show the diff for a change (change id, or url/id for its latest change)
    Diff { target: String },
    /// Mark changes as reviewed (change ids or urls)
    Seen {
        targets: Vec<String>,
        /// Mark everything reviewed
        #[arg(long)]
        all: bool,
    },
    /// Run the scheduler loop (checks due URLs; daily digest notification)
    Daemon {
        /// Local hour (0-23) for the unreviewed-changes digest
        #[arg(long, default_value_t = 9)]
        digest_hour: u32,
    },
    /// Back up and restore the watch list through the cloud
    Sync {
        #[command(subcommand)]
        cmd: Option<SyncCmd>,
    },
    /// Write the watch list to a JSON file (stdout if no path given)
    Export {
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Merge a JSON file from `export` into this database
    Import { path: PathBuf },
}

#[derive(Subcommand)]
enum SyncCmd {
    /// Show whether sync is set up and when it last ran
    Status,
    /// Print what to do to deploy your own backup backend
    Setup {
        /// browser | agent | terminal
        #[arg(long, default_value = "terminal")]
        route: String,
    },
    /// Point this machine at a backend you deployed, after checking it answers
    Connect {
        /// The Worker's URL, e.g. https://litecter-sync.you.workers.dev
        url: String,
    },
    /// Print one string carrying this machine's whole connection, or adopt one
    Link {
        /// Adopt a connection from another machine
        #[arg(long, value_name = "CODE")]
        set: Option<String>,
    },
    /// Ask your backend what version it runs and compare it with this build
    Check,
    /// Print what to do to move your backend to the current worker code
    Update {
        /// browser | agent | terminal
        #[arg(long, default_value = "terminal")]
        route: String,
    },
    /// Write the backend's source — the file you deploy — to stdout or a path
    Worker {
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Stop syncing from this machine. Leaves the watch list and the backup alone
    Disconnect,
    /// Print this machine's sync key — save it in a password manager
    Key {
        /// Adopt an existing key (from another machine) instead of printing
        #[arg(long, value_name = "KEY")]
        set: Option<String>,
        /// Discard the current key and start a new document
        #[arg(long, conflicts_with = "set")]
        reset: bool,
    },
}

fn default_db_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("litecter")
        .join("litecter.db")
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before 1970")
        .as_secs() as i64
}

/// "2h ago" / "in 6d" / "just now"
fn rel_time(ts: i64, now: i64) -> String {
    let (delta, fmt): (i64, fn(String) -> String) = if ts <= now {
        (now - ts, |s| format!("{s} ago"))
    } else {
        (ts - now, |s| format!("in {s}"))
    };
    if delta < 5 {
        return "just now".into();
    }
    let unit = match delta {
        d if d < 60 => format!("{d}s"),
        d if d < 3_600 => format!("{}m", d / 60),
        d if d < 86_400 => format!("{}h", d / 3_600),
        d => format!("{}d", d / 86_400),
    };
    fmt(unit)
}

fn normalize_input_url(raw: &str) -> String {
    let raw = raw.trim();
    if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let db = cli.db.unwrap_or_else(default_db_path);
    let store = Store::open(&db)?;
    let now = now_ts();

    match cli.cmd {
        Cmd::Add { urls, every, selector, from_file } => {
            let schedule: Schedule = every.parse()?;
            let mut all_urls = urls;
            if let Some(path) = from_file {
                let content = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
                all_urls.extend(
                    content
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty() && !l.starts_with('#'))
                        .map(String::from),
                );
            }
            if all_urls.is_empty() {
                bail!("nothing to add — pass URLs or --from-file");
            }
            let mut added = 0;
            for raw in &all_urls {
                let url = normalize_input_url(raw);
                match store.add_url(&url, schedule, selector.as_deref(), now) {
                    Ok(u) => {
                        added += 1;
                        println!("+ #{} {} ({})", u.id, u.url, schedule);
                    }
                    Err(e) => eprintln!("! skipped {url}: {e}"),
                }
            }
            if added > 1 {
                println!("{added} URLs now watched.");
            }
        }

        Cmd::List { json } => {
            let urls = store.list_urls()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&urls)?);
                return Ok(());
            }
            if urls.is_empty() {
                println!("Nothing watched yet. Try: litecter add example.com");
                return Ok(());
            }
            let unseen = store.unseen_url_ids()?;
            println!(
                "{:>4}  {:2} {:8} {:>12} {:>12}  TITLE / URL",
                "ID", "", "EVERY", "LAST CHECK", "NEXT CHECK"
            );
            for u in &urls {
                let marker = if unseen.contains(&u.id) {
                    "*"
                } else if u.status == "error" {
                    "!"
                } else {
                    " "
                };
                let last = u
                    .last_checked_at
                    .map(|t| rel_time(t, now))
                    .unwrap_or_else(|| "never".into());
                let next = rel_time(u.next_check_at, now);
                let label = match &u.title {
                    Some(t) => format!("{t} — {}", u.url),
                    None => u.url.clone(),
                };
                println!("{:>4}  {:2} {:8} {:>12} {:>12}  {}", u.id, marker, u.schedule.to_string(), last, next, label);
                if u.status == "error"
                    && let Some(msg) = &u.error_message
                {
                    println!("{:44}└ error: {}", "", msg);
                }
            }
            let n = store.count_unseen()?;
            if n > 0 {
                println!("\n{n} unreviewed change(s) — litecter changes");
            }
        }

        Cmd::Rm { targets } => {
            if targets.is_empty() {
                bail!("pass ids or urls to remove");
            }
            for t in &targets {
                match store.resolve_url(t)? {
                    Some(u) => {
                        store.remove_url(u.id, now)?;
                        println!("- removed #{} {}", u.id, u.url);
                    }
                    None => eprintln!("! not watched: {t}"),
                }
            }
        }

        Cmd::Check { all, targets } => {
            let list: Vec<UrlRow> = if !targets.is_empty() {
                let mut v = Vec::new();
                for t in &targets {
                    match store.resolve_url(t)? {
                        Some(u) => v.push(u),
                        None => eprintln!("! not watched: {t} (litecter add {t})"),
                    }
                }
                v
            } else if all {
                store.list_urls()?
            } else {
                store.due_urls(now)?
            };

            if list.is_empty() {
                println!("Nothing due. (litecter check --all to force)");
                return Ok(());
            }

            // Wake → check → sleep: the browser exists only for this batch.
            eprintln!("Checking {} URL(s)…", list.len());
            let renderer = Renderer::launch().await?;
            let (mut changed, mut errors) = (0, 0);
            for u in &list {
                let result = check_one(&store, &renderer, u, now_ts()).await;
                let label = u.title.as_deref().unwrap_or(&u.url);
                match result {
                    CheckResult::Unchanged => println!("  = {label} — unchanged"),
                    CheckResult::Baseline => println!("  + {label} — first snapshot (baseline)"),
                    CheckResult::Changed { added, removed } => {
                        changed += 1;
                        println!("  * {label} — CHANGED +{added} −{removed}");
                    }
                    CheckResult::Reverted => println!("  = {label} — reverted to last-seen state"),
                    CheckResult::Errored(msg) => {
                        errors += 1;
                        println!("  ! {label} — {msg}");
                    }
                }
            }
            renderer.shutdown().await;
            println!(
                "{} checked · {changed} changed · {errors} error(s)",
                list.len()
            );
            if changed > 0 {
                println!("Review with: litecter changes");
            }
        }

        Cmd::Changes { unseen, json } => {
            let changes = store.list_changes(unseen)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&changes)?);
                return Ok(());
            }
            if changes.is_empty() {
                println!("No {}changes.", if unseen { "unreviewed " } else { "" });
                return Ok(());
            }
            for c in &changes {
                let marker = if c.seen_at.is_none() { "*" } else { " " };
                let label = c.title.as_deref().unwrap_or(&c.url);
                println!(
                    "{marker} #{:<4} {}  +{} −{}  {}",
                    c.id,
                    label,
                    c.lines_added,
                    c.lines_removed,
                    rel_time(c.detected_at, now)
                );
                if let Some(snippet) = &c.snippet {
                    println!("        \"{snippet}\"");
                }
            }
            let n = store.count_unseen()?;
            println!("\n{n} unreviewed. litecter diff <id> · litecter seen <id|--all>");
        }

        Cmd::Diff { target } => {
            let change = match target.trim_start_matches('#').parse::<i64>() {
                Ok(id) => match store.change_by_id(id)? {
                    Some(c) => Some(c),
                    None => match store.resolve_url(&target)? {
                        Some(u) => store.latest_change_for_url(u.id)?,
                        None => None,
                    },
                },
                Err(_) => match store.resolve_url(&target)? {
                    Some(u) => store.latest_change_for_url(u.id)?,
                    None => None,
                },
            };
            let Some(c) = change else {
                bail!("no change found for '{target}'");
            };
            let from = store.snapshot_text(c.from_snapshot_id)?;
            let to = store.snapshot_text(c.to_snapshot_id)?;
            let label = c.title.as_deref().unwrap_or(&c.url);
            println!(
                "# {} — +{} −{} · detected {} · {}\n",
                label,
                c.lines_added,
                c.lines_removed,
                rel_time(c.detected_at, now),
                c.url
            );
            let diff = differ::unified(&from, &to, 3);
            if std::io::stdout().is_terminal() {
                for line in diff.lines() {
                    let colored = match line.as_bytes().first() {
                        Some(b'+') => format!("\x1b[32m{line}\x1b[0m"),
                        Some(b'-') => format!("\x1b[31m{line}\x1b[0m"),
                        Some(b'@') => format!("\x1b[36m{line}\x1b[0m"),
                        _ => line.to_string(),
                    };
                    println!("{colored}");
                }
            } else {
                println!("{diff}");
            }
            if c.seen_at.is_none() {
                println!("\nMark reviewed: litecter seen {}", c.id);
            }
        }

        Cmd::Seen { targets, all } => {
            if all {
                let n = store.mark_all_seen(now)?;
                println!("Marked {n} change(s) reviewed.");
                return Ok(());
            }
            if targets.is_empty() {
                bail!("pass change ids/urls, or --all");
            }
            for t in &targets {
                let change = match t.trim_start_matches('#').parse::<i64>() {
                    Ok(id) => store.change_by_id(id)?,
                    Err(_) => match store.resolve_url(t)? {
                        Some(u) => store.latest_change_for_url(u.id)?,
                        None => None,
                    },
                };
                match change {
                    Some(c) if c.seen_at.is_none() => {
                        store.mark_seen(c.id, now)?;
                        println!("seen: #{} {}", c.id, c.title.as_deref().unwrap_or(&c.url));
                    }
                    Some(_) => println!("already seen: {t}"),
                    None => eprintln!("! no change found for '{t}'"),
                }
            }
        }

        Cmd::Daemon { digest_hour } => {
            if digest_hour > 23 {
                bail!("--digest-hour must be 0-23");
            }
            let notify = |title: &str, body: &str| {
                if let Err(e) = notify_rust::Notification::new()
                    .summary(title)
                    .body(body)
                    .appname("Litecter")
                    .show()
                {
                    eprintln!("notification failed: {e}");
                }
            };
            run_daemon(&store, DaemonOptions { digest_hour }, notify).await?;
        }

        Cmd::Sync { cmd } => match cmd {
            None => run_sync(&store, now).await?,
            Some(SyncCmd::Status) => sync_status(&store, now).await?,
            Some(SyncCmd::Setup { route }) => sync_setup(&store, &route, now)?,
            Some(SyncCmd::Connect { url }) => sync_connect(&store, &url, now).await?,
            Some(SyncCmd::Link { set }) => sync_link(&store, set, now)?,
            Some(SyncCmd::Check) => {
                sync_check(&store).await?;
            }
            Some(SyncCmd::Update { route }) => sync_update(&store, &route).await?,
            Some(SyncCmd::Worker { out }) => match out {
                Some(path) => {
                    std::fs::write(&path, sync::worker::SOURCE)
                        .with_context(|| format!("writing {}", path.display()))?;
                    eprintln!("Wrote the backend to {}", path.display());
                }
                None => print!("{}", sync::worker::SOURCE),
            },
            Some(SyncCmd::Disconnect) => {
                sync::disconnect(&store, now)?;
                println!("Disconnected. The watch list is untouched, and so is the backup —");
                println!("delete that from your own Cloudflare dashboard if you want it gone.");
            }
            Some(SyncCmd::Key { set, reset }) => sync_key(&store, set, reset, now)?,
        },

        // Export/import reuse the sync document, so a file moved by hand
        // carries exactly what the cloud path carries — including the
        // unreviewed inbox — and merges by the same rules.
        Cmd::Export { out } => {
            let doc = sync::SyncDoc::build(&store, now)?;
            let json = serde_json::to_string_pretty(&doc)?;
            match out {
                Some(path) => {
                    std::fs::write(&path, &json)
                        .with_context(|| format!("writing {}", path.display()))?;
                    eprintln!("Exported {} URL(s) to {}", doc.urls.len(), path.display());
                }
                None => println!("{json}"),
            }
        }

        Cmd::Import { path } => {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let incoming: sync::SyncDoc =
                serde_json::from_str(&raw).context("that file is not a Litecter export")?;
            let merged = sync::doc::merge(sync::SyncDoc::build(&store, now)?, incoming);
            let stats = sync::doc::apply(&store, &merged, now)?;
            println!(
                "Imported — {} added, {} updated, {} removed, {} unreviewed change(s) restored.",
                stats.added, stats.updated, stats.removed, stats.pendings_restored
            );
        }
    }

    Ok(())
}

// ---- sync ---------------------------------------------------------------------

async fn run_sync(store: &Store, now: i64) -> Result<()> {
    if !sync::is_configured(store)? {
        bail!(
            "backup is not set up on this machine.\n\n\
             Litecter runs no sync service — you deploy a small backend to your own\n\
             Cloudflare account, on the free plan, and it stays yours. Start with:\n\n    \
             litecter sync setup\n\n\
             Already have one on another machine?  litecter sync link --set <code>"
        );
    }

    let report = sync::sync_now(store, now).await?;
    let s = &report.stats;

    println!(
        "Synced {} URL(s) — {} added, {} updated, {} removed locally.",
        report.urls_in_document, s.added, s.updated, s.removed
    );
    if s.pendings_restored > 0 {
        println!("Restored {} unreviewed change(s) — litecter changes", s.pendings_restored);
    }
    if report.diffs_dropped_for_size > 0 {
        println!(
            "! {} diff(s) were too large to upload; those pages will re-baseline on their next check.",
            report.diffs_dropped_for_size
        );
    }
    if report.attempts > 1 {
        println!("({} attempts — another device was syncing at the same time)", report.attempts);
    }
    println!("Uploaded {:.1} KB.", report.uploaded_bytes as f64 / 1024.0);
    Ok(())
}

async fn sync_status(store: &Store, now: i64) -> Result<()> {
    if !sync::is_configured(store)? {
        if sync::load_key(store)?.is_some() {
            // Setup got as far as generating a key and stopped. Say which half
            // is missing rather than "not set up", which sends people back to
            // the start of a wizard they already finished most of.
            println!("Backup is half set up: this machine has a key but no backend to send it to.");
            println!("Finish with:  litecter sync connect <your worker URL>");
        } else {
            println!("Backup is not set up. Start with:  litecter sync setup");
        }
        return Ok(());
    }
    // A failure recorded by the daemon or the desktop app has to be visible
    // here too — the whole point is that it can't be missed — and it leads,
    // because a stale "last synced" line reads like good news on its own.
    let health = sync::health(store)?;
    if let Some(since) = health.failing_since {
        println!("⚠ Backup is failing — nothing has synced since {}", rel_time(since, now));
        if let Some(err) = &health.last_error {
            println!("  {err}");
        }
        println!("  Retry with `litecter sync`.\n");
    }

    println!("Backend:     {}", sync::endpoint(store)?.unwrap_or_default());
    match sync::last_synced_at(store)? {
        Some(ts) => println!("Last synced: {}", rel_time(ts, now)),
        None => println!("Last synced: never (connected, but no sync has completed)"),
    }
    if health.failing_since.is_some() {
        return Ok(());
    }
    let doc = sync::SyncDoc::build(store, now)?;
    let pendings = doc.urls.iter().filter(|u| u.pending.is_some()).count();
    println!("Would send:  {} URL(s), {pendings} unreviewed change(s)", doc.urls.len());

    // Best-effort: a status command should not fail because the network is
    // down, and an unreachable backend says nothing about its version.
    if let Ok(check) = sync_check_quiet(store).await
        && check.is_outdated()
    {
        let deployed = if check.deployed == sync::worker::PRE_VERSIONING {
            "a version from before Litecter tracked them".to_string()
        } else {
            format!("v{}", check.deployed)
        };
        println!(
            "\n! Your backend runs {deployed} and this build ships v{}. Everything keeps",
            check.bundled.unwrap_or_default(),
        );
        println!("  working; update it when convenient:  litecter sync update");
    }
    println!("\nMove this connection to another machine:  litecter sync link");
    Ok(())
}

// ---- backend setup -------------------------------------------------------------

fn parse_route(raw: &str) -> Result<sync::setup::Route> {
    sync::setup::Route::parse(raw)
        .with_context(|| format!("`{raw}` is not a route — pick browser, agent or terminal"))
}

fn sync_setup(store: &Store, route: &str, now: i64) -> Result<()> {
    let route = parse_route(route)?;
    // The token has to exist before the backend can be told about it, so this
    // is where the key is born — not on the first sync.
    let key = sync::ensure_key(store, now)?;

    println!("Litecter runs no sync service. You deploy a small backend — one file, no");
    println!("dependencies — to your own Cloudflare account, and it stays yours. The free");
    println!("plan covers this many times over.\n");

    if route == sync::setup::Route::Browser {
        println!("Worker source:  litecter sync worker --out {}\n", sync::worker::ASSET_NAME);
        println!("Token for step 5 (SYNC_TOKEN):\n\n  {}\n", key.auth_token());
    }
    println!("{}\n", sync::setup::setup(route, &key.auth_token()));

    if route.setup_carries_secret() {
        println!("^ The text above contains this machine's token. It is the one secret here;");
        println!("  paste it into your own shell or agent and nowhere else.\n");
    }
    println!("Then:  litecter sync connect <the URL it printed>");
    print_key_banner(store)
}

async fn sync_connect(store: &Store, url: &str, now: i64) -> Result<()> {
    let endpoint = sync::link::normalize_endpoint(url)?;
    let key = sync::ensure_key(store, now)?;

    // Verify before saving. The app cannot see the user's Cloudflare account,
    // so proving the connection from this side is the only check that means
    // anything — "the instructions said it worked" is not evidence.
    print!("Checking {endpoint} … ");
    use std::io::Write;
    std::io::stdout().flush().ok();
    let meta = sync::Connection { endpoint: endpoint.clone(), key }.verify().await?;
    println!("ok (v{}).", meta.version);

    sync::save_endpoint(store, &endpoint, now)?;
    println!("Connected. Run `litecter sync` to make the first backup.");
    Ok(())
}

fn sync_link(store: &Store, set: Option<String>, now: i64) -> Result<()> {
    if let Some(code) = set {
        sync::adopt_link(store, &code, now)?;
        match sync::endpoint(store)? {
            Some(e) => println!("Adopted. Backend: {e}\nRun `litecter sync` to pull the list."),
            None => println!(
                "Key adopted, but that code carried no backend address.\n\
                 Add it with:  litecter sync connect <url>"
            ),
        }
        return Ok(());
    }
    let code = sync::link_code(store)?
        .context("backup is not set up on this machine — run `litecter sync setup`")?;
    println!("\n  {code}\n");
    println!("Paste that on the other machine:  litecter sync link --set '<code>'");
    println!("\nIt contains this machine's sync key. Treat it like a password: it is what");
    println!("decrypts the backup, and nobody can recover it for you.");
    Ok(())
}

/// Probe without printing. Callers that want the failure reported should use
/// [`sync_check`].
async fn sync_check_quiet(store: &Store) -> Result<sync::WorkerCheck> {
    sync::Connection::load(store)?
        .context("backup is not set up on this machine — run `litecter sync setup`")?
        .probe()
        .await
}

async fn sync_check(store: &Store) -> Result<sync::WorkerCheck> {
    let check = sync_check_quiet(store).await?;
    let bundled = check.bundled.context("this build could not read its own worker version")?;
    if check.deployed == sync::worker::PRE_VERSIONING {
        println!("Your backend predates version reporting; this build ships v{bundled}.");
    } else {
        println!("Backend v{}, this build ships v{bundled}.", check.deployed);
    }
    if check.is_outdated() {
        println!("\nBackups keep working while it is behind. Update it with:");
        println!("  litecter sync update");
    } else {
        println!("Up to date.");
    }
    Ok(check)
}

async fn sync_update(store: &Store, route: &str) -> Result<()> {
    let route = parse_route(route)?;
    let endpoint = sync::endpoint(store)?
        .context("backup is not set up on this machine — run `litecter sync setup`")?;
    let deployment = sync::setup::Deployment::from_endpoint(&endpoint);

    // Ask rather than assume: whether the extra secret step is needed depends
    // on what is actually deployed, and getting it wrong either omits a step
    // the user needs or adds one that confuses them.
    let needs_token = match sync_check_quiet(store).await {
        Ok(check) if !check.is_outdated() => {
            println!("Your backend is already current (v{}). Nothing to do.\n", check.deployed);
            return Ok(());
        }
        Ok(check) => check.needs_token_secret(),
        Err(e) => {
            eprintln!("! Could not reach the backend to check its version: {e:#}");
            eprintln!("  Printing the update steps anyway.\n");
            false
        }
    };

    if route == sync::setup::Route::Browser {
        println!("Worker source:  litecter sync worker --out {}\n", sync::worker::ASSET_NAME);
    }
    println!("{}\n", sync::setup::update(route, &deployment, needs_token));
    println!("Then confirm it landed:  litecter sync check");
    Ok(())
}

/// The one moment the key is worth shouting about: it is the only way back to
/// the backup, and nothing else can recover it.
fn print_key_banner(store: &Store) -> Result<()> {
    let key = sync::load_key(store)?.context("no sync key configured")?;
    println!("\n  Sync key:  {}\n", key.encode());
    println!("Save this in your password manager. It is the only way to read the backup,");
    println!("and nobody can recover it for you — not us, and not Cloudflare, who only");
    println!("ever hold bytes this key encrypted.");
    println!("\nMoving to another machine? `litecter sync link` carries this and the");
    println!("backend address in one string.");
    Ok(())
}

fn sync_key(store: &Store, set: Option<String>, reset: bool, now: i64) -> Result<()> {
    if let Some(raw) = set {
        let key = sync::SyncKey::decode(&raw).context("that does not look like a sync key")?;
        sync::save_key(store, &key, now)?;
        println!("Key adopted. It still needs a backend to talk to:");
        println!("  litecter sync connect <url>");
        return Ok(());
    }
    if reset {
        let key = sync::SyncKey::generate()?;
        sync::save_key(store, &key, now)?;
        println!("New key generated — the previous backup is no longer readable from here.");
        println!("Your backend also needs its SYNC_TOKEN secret replaced with the new token,");
        println!("or every sync from here will be rejected. The new token is:\n");
        println!("  {}\n", key.auth_token());
        return print_key_banner(store);
    }
    if sync::load_key(store)?.is_none() {
        bail!("no sync key yet — run `litecter sync setup` to create one");
    }
    print_key_banner(store)
}
