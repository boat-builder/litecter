use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use litecter_core::{
    check_one, differ, run_daemon, CheckResult, DaemonOptions, Renderer, Schedule, Store, UrlRow,
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
                "{:>4}  {:2} {:8} {:>12} {:>12}  {}",
                "ID", "", "EVERY", "LAST CHECK", "NEXT CHECK", "TITLE / URL"
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
                if u.status == "error" {
                    if let Some(msg) = &u.error_message {
                        println!("{:44}└ error: {}", "", msg);
                    }
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
                        store.remove_url(u.id)?;
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
    }

    Ok(())
}
