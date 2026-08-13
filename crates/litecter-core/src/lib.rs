//! litecter-core — the engine behind Litecter.
//!
//! Everything lives here so the CLI (`litecter`) and the desktop app share
//! identical behavior: SQLite store, headless-Chromium renderer, text
//! normalization, diffing, and the check pipeline.

pub mod checker;
pub mod differ;
pub mod renderer;
pub mod schedule;
pub mod scheduler;
pub mod store;
pub mod sync;
pub mod textproc;

pub use checker::{check_one, fetch_rendered, persist_check, CheckResult, Fetched};
pub use scheduler::{run_daemon, DaemonOptions};
pub use renderer::{RenderedPage, Renderer};
pub use schedule::Schedule;
pub use store::{ChangeItem, Store, UrlConfig, UrlRow};
pub use sync::{sync_now, SyncKey, SyncReport};
