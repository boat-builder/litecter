//! HTTP transport for the sync endpoint.
//!
//! Deliberately thin: pull bytes, push bytes, surface the ETag. Everything
//! meaningful — what the bytes are, whether they can be read, how two versions
//! reconcile — lives in [`super::crypto`] and [`super::doc`].

use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::{Client, StatusCode};

use super::worker::PRE_VERSIONING;

const TIMEOUT: Duration = Duration::from_secs(30);

/// Verifying a pasted address should not make someone wait the full sync
/// timeout to learn they typed it wrong.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// A document as it exists on the server, with the ETag needed to replace it.
pub struct Remote {
    pub sealed: Vec<u8>,
    pub etag: String,
}

/// Which half of setup is asking for a connection to be checked.
///
/// It exists for exactly one status code. A 401 means opposite things on the two
/// paths, and the advice that fits one is destructive on the other: telling
/// someone restoring an existing backup to change their backend's `SYNC_TOKEN`
/// strands the backup they were trying to reach — the object is stored under a
/// hash of that token, and the key they replaced was the only thing that could
/// decrypt it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Intent {
    /// An address typed into setup, checked with whatever key this machine has.
    Connecting,
    /// A connection string pasted from a machine that already backs up.
    Adopting,
}

pub struct SyncClient {
    http: Client,
    endpoint: String,
    token: String,
}

impl SyncClient {
    pub fn new(endpoint: &str, token: String) -> Result<Self> {
        let http = Client::builder()
            .timeout(TIMEOUT)
            .user_agent(concat!("litecter/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building HTTP client")?;
        Ok(Self { http, endpoint: endpoint.trim_end_matches('/').to_string(), token })
    }

    fn blob_url(&self) -> String {
        format!("{}/v1/blob", self.endpoint)
    }

    /// What version the user's deployment is running.
    ///
    /// Three outcomes, and the third is the one that is easy to get wrong:
    ///
    /// - `200` — the deployment answered with its version.
    /// - `404` — the deployment answered, but predates this route, so it is
    ///   [`PRE_VERSIONING`]. Old, but positively identified.
    /// - anything else — **error**. A laptop on a plane, an expired token or a
    ///   Cloudflare hiccup must leave the version *unknown*, because folding
    ///   that into "outdated" means nagging every offline user to redeploy a
    ///   worker that was already current.
    pub async fn meta(&self) -> Result<WorkerMeta> {
        match self.fetch_meta().await? {
            MetaOutcome::Meta(meta) => Ok(meta),
            MetaOutcome::PreVersioning => {
                Ok(WorkerMeta { version: PRE_VERSIONING, features: Vec::new() })
            }
            MetaOutcome::Refused(status) => {
                bail!("your backup backend returned {status}{}", detail(status))
            }
        }
    }

    /// The three outcomes above, before they are flattened into a version.
    ///
    /// [`verify`](Self::verify) needs to tell a rejected token apart from every
    /// other failure, and it cannot do that once `meta` has turned the status
    /// into prose.
    async fn fetch_meta(&self) -> Result<MetaOutcome> {
        let response = self
            .http
            .get(format!("{}/v1/meta", self.endpoint))
            .timeout(PROBE_TIMEOUT)
            .bearer_auth(&self.token)
            .send()
            .await
            .context("contacting your backup backend")?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("reading the backend's version")
                .map(MetaOutcome::Meta),
            StatusCode::NOT_FOUND => Ok(MetaOutcome::PreVersioning),
            status => Ok(MetaOutcome::Refused(status)),
        }
    }

    /// Check an address and token before saving them.
    ///
    /// Setup ends here rather than at "the instructions said it worked": the
    /// app cannot see the user's Cloudflare account, so proving the connection
    /// from this side is the only verification that means anything.
    pub async fn verify(&self, intent: Intent) -> Result<WorkerMeta> {
        match self.fetch_meta().await? {
            MetaOutcome::Meta(meta) if meta.version != PRE_VERSIONING => Ok(meta),
            // A 404 is ambiguous in a way it is not during a routine probe: it
            // is either a worker deployed before `/v1/meta` existed, or an
            // address that has nothing to do with Litecter. `/v1/health` tells
            // the two apart, and the difference is the whole error message.
            MetaOutcome::Meta(_) | MetaOutcome::PreVersioning => match self.health().await {
                Ok(true) => bail!(
                    "that backend is running a version from before Litecter checked versions. \
                     Redeploy it with the current worker code, then connect again."
                ),
                _ => bail!(
                    "that address answered, but not like a Litecter backend. \
                     Check you pasted the Worker's URL and not something else."
                ),
            },
            MetaOutcome::Refused(StatusCode::UNAUTHORIZED) => bail!("{}", rejected(intent)),
            MetaOutcome::Refused(status) => {
                bail!("your backup backend returned {status}{}", detail(status))
            }
        }
    }

    /// Erase the stored document.
    ///
    /// The app has to be the one to do this: R2 refuses to delete a bucket that
    /// still has objects in it, and neither wrangler nor the dashboard will
    /// empty one for you. So "remove my backend" starts here, holding the only
    /// token that can reach the object.
    pub async fn delete(&self) -> Result<()> {
        let response = self
            .http
            .delete(self.blob_url())
            .bearer_auth(&self.token)
            .send()
            .await
            .context("contacting your backup backend")?;
        match response.status() {
            // Already gone is the outcome we wanted.
            StatusCode::OK | StatusCode::NOT_FOUND => Ok(()),
            status => bail!("your backup backend returned {status}{}", detail(status)),
        }
    }

    /// Unauthenticated liveness. `Ok(false)` means something answered and it
    /// was not a Litecter backend.
    async fn health(&self) -> Result<bool> {
        let response = self
            .http
            .get(format!("{}/v1/health", self.endpoint))
            .timeout(PROBE_TIMEOUT)
            .send()
            .await
            .context("contacting your backup backend")?;
        if !response.status().is_success() {
            return Ok(false);
        }
        Ok(response.json::<Health>().await.map(|h| h.ok).unwrap_or(false))
    }

    /// `Ok(None)` means "no document yet" — a first sync, not a failure.
    pub async fn pull(&self) -> Result<Option<Remote>> {
        let response = self
            .http
            .get(self.blob_url())
            .bearer_auth(&self.token)
            .send()
            .await
            .context("contacting the sync endpoint")?;

        match response.status() {
            StatusCode::NOT_FOUND => Ok(None),
            StatusCode::OK => {
                let etag = response
                    .headers()
                    .get(reqwest::header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let sealed = response.bytes().await.context("reading sync document")?.to_vec();
                Ok(Some(Remote { sealed, etag }))
            }
            status => bail!("sync endpoint returned {status} on pull{}", detail(status)),
        }
    }

    /// Conditional replace. `etag` is what [`pull`](Self::pull) returned, or
    /// `None` when there was no document. Returns the new ETag, or `Ok(None)`
    /// if another device won the race and the caller should re-pull and retry.
    pub async fn push(&self, sealed: &[u8], etag: Option<&str>) -> Result<Option<String>> {
        let mut request = self
            .http
            .put(self.blob_url())
            .bearer_auth(&self.token)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(sealed.to_vec());
        if let Some(etag) = etag {
            request = request.header(reqwest::header::IF_MATCH, etag);
        }

        let response = request.send().await.context("uploading sync document")?;
        match response.status() {
            StatusCode::PRECONDITION_FAILED => Ok(None),
            StatusCode::OK => {
                #[derive(serde::Deserialize)]
                struct Body {
                    etag: String,
                }
                let body: Body = response.json().await.context("reading push response")?;
                Ok(Some(body.etag))
            }
            status => bail!("sync endpoint returned {status} on push{}", detail(status)),
        }
    }
}

/// What the deployment reports about itself.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct WorkerMeta {
    pub version: u32,
    /// Alongside the version so the app can ask "can it do X" without having to
    /// memorise which release added what.
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(serde::Deserialize)]
struct Health {
    ok: bool,
}

/// What `/v1/meta` said, before it is judged.
enum MetaOutcome {
    Meta(WorkerMeta),
    /// Answered, but predates the route — old, and positively identified.
    PreVersioning,
    Refused(StatusCode),
}

/// The message for a token the backend would not accept, during setup.
///
/// Setup is the one place where "make the backend's secret match this machine"
/// is the wrong instruction often enough to be dangerous, so each path names its
/// own likely mistake and the recovery that does not cost a backup.
fn rejected(intent: Intent) -> &'static str {
    match intent {
        Intent::Connecting => {
            "that backend rejected this machine's token. If you are restoring a backup made on \
             another machine, go back and choose “Restore an existing backup” — its connection \
             string is the only thing that can decrypt it, and changing the backend's SYNC_TOKEN \
             to match this machine would strand it. If you just deployed this backend, set its \
             SYNC_TOKEN secret to the token shown above and deploy again."
        }
        Intent::Adopting => {
            "that backend rejected the key in this connection string. Check you pasted all of it, \
             and that it came from the machine that backs up to this address."
        }
    }
}

/// Turn the handful of statuses a user can actually act on into advice.
///
/// These read differently now that the backend belongs to the user: a 401 is no
/// longer "your key is wrong", it is "the two halves of your own deployment
/// disagree", and that is a thing they can go and fix.
fn detail(status: StatusCode) -> &'static str {
    match status {
        StatusCode::UNAUTHORIZED => {
            " — the backend rejected this machine's key. Its SYNC_TOKEN secret \
             must match the token Litecter shows in Settings → Backup."
        }
        StatusCode::SERVICE_UNAVAILABLE => {
            " — the backend has no SYNC_TOKEN secret set. Add it to the Worker \
             and deploy again."
        }
        StatusCode::PAYLOAD_TOO_LARGE => " — the document exceeds the 8 MB limit",
        s if s.is_server_error() => " — the backend is having trouble; this usually resolves itself",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejected_token_during_setup_offers_the_restore_path() {
        // The likeliest reason a fresh machine is refused by a backend that
        // exists is that the backup already there was made with another key.
        // "Change SYNC_TOKEN to match this machine" is the advice that loses it,
        // so the restore route has to be named first.
        let msg = rejected(Intent::Connecting);
        assert!(msg.contains("Restore an existing backup"), "got: {msg}");
        let restore = msg.find("Restore an existing backup").unwrap();
        let secret = msg.find("SYNC_TOKEN").unwrap();
        assert!(restore < secret, "the recoverable option has to come first: {msg}");
    }

    #[test]
    fn a_rejected_key_while_adopting_never_suggests_changing_the_secret() {
        // Here the user pasted a key, so a 401 means the paste is wrong — not
        // the deployment. Sending them to redeploy the backend would break a
        // backup that is working for the machine they copied from.
        let msg = rejected(Intent::Adopting);
        assert!(!msg.contains("SYNC_TOKEN"), "got: {msg}");
    }
}
