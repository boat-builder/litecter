//! The backend the user deploys, and how we know when theirs has fallen behind.
//!
//! Litecter does not run a sync server. Each user deploys `worker/src/index.js`
//! to their own Cloudflare account, which buys no accounts, no data custody and
//! no bill — and costs exactly one thing: **we can never deploy anything
//! again.** Every design choice below falls out of that.
//!
//! The consequence that matters is that a user's deployment can be arbitrarily
//! old, and that is normal rather than an error. So the worker carries a version
//! integer, the app ships a copy of the same file, and the two are compared. The
//! version number is the whole protocol; everything else is delivery.
//!
//! Note what is *not* here: a hard-coded "latest is version 3". The app parses
//! the constant back out of the source it ships, so bumping the worker ships to
//! the app in the same commit and the two cannot drift apart silently.

use std::sync::OnceLock;

use regex::Regex;

/// The backend, verbatim. This is the same file the release attaches and the
/// same bytes the "copy worker code" button hands over, which is what makes
/// [`bundled_version`] trustworthy.
///
/// It is plain JavaScript with no dependencies and no build step precisely so
/// that this can be an `include_str!` rather than a bundler: the artifact a user
/// pastes into their own cloud account should be something they can read.
pub const SOURCE: &str = include_str!("../../../../worker/src/index.js");

pub const REPO_URL: &str = "https://github.com/boat-builder/litecter";

/// Stable alias for the newest worker. Deliberately `latest` rather than a
/// pinned tag: a newer worker always works with an older app (the API only
/// grows), so someone who deploys today and updates Litecter next month is fine.
pub const RELEASE_URL: &str =
    "https://github.com/boat-builder/litecter/releases/latest/download/litecter-worker.js";

/// The filename the release asset carries; also what the terminal route writes.
pub const ASSET_NAME: &str = "litecter-worker.js";

/// What we suggest naming things, and what we assume when we have to guess.
pub const DEFAULT_WORKER_NAME: &str = "litecter-sync";
pub const DEFAULT_BUCKET_NAME: &str = "litecter-sync";

/// The name of the secret holding the bearer token.
pub const TOKEN_SECRET_NAME: &str = "SYNC_TOKEN";

/// The R2 binding the worker expects.
pub const BUCKET_BINDING: &str = "SYNC";

/// A deployment that predates `/v1/meta`, identified by a 404 on that route.
///
/// This is a *positive* identification — the deployment answered, it is simply
/// old — which is why it is a real version number and not an `Option`. Do not
/// use it for "we could not reach it"; see [`crate::sync::client::SyncClient::meta`].
pub const PRE_VERSIONING: u32 = 0;

/// Pull `WORKER_VERSION` out of worker source.
///
/// Returns `None` when the constant cannot be found, and callers must treat that
/// as "do not nag" rather than inventing a number. A parse that silently
/// returned 0 would mark every healthy deployment as ahead of us, which is a
/// worse failure than never mentioning updates at all.
pub fn parse_version(source: &str) -> Option<u32> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?m)^\s*const\s+WORKER_VERSION\s*=\s*(\d+)\s*;").expect("valid regex")
    });
    re.captures(source)?.get(1)?.as_str().parse().ok()
}

/// The version of the worker this build of Litecter ships.
pub fn bundled_version() -> Option<u32> {
    static VERSION: OnceLock<Option<u32>> = OnceLock::new();
    *VERSION.get_or_init(|| parse_version(SOURCE))
}

/// A deployment's identity as best we can determine it from its URL.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkerName {
    pub name: String,
    /// True when we inferred the name from a convention rather than read it.
    ///
    /// This flag is load-bearing, not decoration. Deploying under a name that
    /// does not exist does not error — it silently creates a *second* worker
    /// while the real one keeps serving old code — and deploying under a name
    /// that belongs to something else silently overwrites it. So a guess is
    /// marked as a guess everywhere it is shown, the agent prompt is told to
    /// confirm it before mutating anything, and the generated script refuses to
    /// run rather than risk either.
    pub guessed: bool,
}

/// Recover the deployment name from its endpoint.
///
/// A `*.workers.dev` URL carries the worker name as its first label, so that
/// case is certain. Behind a custom domain the hostname says nothing about what
/// the deployment is called, and all we can do is assume the name we suggested
/// at setup — hence `guessed`.
pub fn name_from_endpoint(endpoint: &str) -> WorkerName {
    if let Some(label) = host_of(endpoint)
        .strip_suffix(".workers.dev")
        .and_then(|h| h.split('.').next())
        && !label.is_empty()
    {
        return WorkerName { name: label.to_string(), guessed: false };
    }
    WorkerName { name: DEFAULT_WORKER_NAME.to_string(), guessed: true }
}

/// The hostname out of an endpoint, with scheme, path and port stripped.
pub fn host_of(endpoint: &str) -> &str {
    let trimmed = endpoint.trim().trim_end_matches('/');
    let after_scheme = trimmed.split_once("://").map_or(trimmed, |(_, rest)| rest);
    after_scheme
        .split('/')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
}

/// Deployed version against the one we ship.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkerCheck {
    pub deployed: u32,
    /// `None` when the shipped source could not be parsed — see [`parse_version`].
    pub bundled: Option<u32>,
    pub features: Vec<String>,
}

impl WorkerCheck {
    pub fn is_outdated(&self) -> bool {
        self.bundled.is_some_and(|bundled| self.deployed < bundled)
    }

    /// A pre-versioning deployment has no `SYNC_TOKEN` secret, because the
    /// check did not exist when it was deployed. Updating the code alone would
    /// take it from working to 503, so this case earns an extra step in the
    /// instructions rather than a nasty surprise.
    pub fn needs_token_secret(&self) -> bool {
        self.deployed == PRE_VERSIONING
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_worker_declares_a_version() {
        // If this fails the update nag silently disables itself, so it is worth
        // a test rather than a runtime shrug.
        assert!(bundled_version().is_some(), "WORKER_VERSION missing from worker/src/index.js");
    }

    #[test]
    fn version_is_read_from_source_not_declared_here() {
        assert_eq!(parse_version("const WORKER_VERSION = 7;"), Some(7));
        assert_eq!(parse_version("  const WORKER_VERSION=12 ;"), Some(12));
    }

    #[test]
    fn an_unreadable_version_disables_the_nag_rather_than_inventing_one() {
        assert_eq!(parse_version("const WORKER_VERSION = 'oops';"), None);
        assert_eq!(parse_version("nothing here"), None);
        // A mention in a comment must not be mistaken for the declaration.
        assert_eq!(parse_version("// see const WORKER_VERSION = 3; above"), None);
    }

    #[test]
    fn workers_dev_urls_identify_the_deployment_exactly() {
        for url in [
            "https://litecter-sync.alice.workers.dev",
            "https://litecter-sync.alice.workers.dev/",
            "litecter-sync.alice.workers.dev",
        ] {
            let n = name_from_endpoint(url);
            assert_eq!(n.name, "litecter-sync", "{url}");
            assert!(!n.guessed, "{url} names the worker outright");
        }
    }

    #[test]
    fn a_custom_domain_can_only_be_guessed_at() {
        let n = name_from_endpoint("https://sync.example.com");
        assert_eq!(n.name, DEFAULT_WORKER_NAME);
        assert!(n.guessed, "a custom domain says nothing about the worker's name");
    }

    #[test]
    fn outdated_needs_a_number_on_both_sides() {
        let check = |deployed, bundled| WorkerCheck { deployed, bundled, features: vec![] };
        assert!(check(1, Some(2)).is_outdated());
        assert!(!check(2, Some(2)).is_outdated());
        assert!(!check(3, Some(2)).is_outdated(), "newer than us is fine, not a nag");
        assert!(!check(1, None).is_outdated(), "unparseable bundle must not nag");
    }

    #[test]
    fn a_pre_versioning_deployment_is_outdated_and_needs_its_secret() {
        let check = WorkerCheck { deployed: PRE_VERSIONING, bundled: Some(1), features: vec![] };
        assert!(check.is_outdated());
        assert!(check.needs_token_secret());
    }
}
