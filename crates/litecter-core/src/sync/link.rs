//! Moving a connection to a second machine in one paste.
//!
//! Self-hosting means a connection is now *two* values — the sync key, which is
//! the secret, and the endpoint, which is where that user's backend happens to
//! live. Asking someone to transcribe two things from one machine to another is
//! how a restore turns into a support thread, so they travel together:
//!
//! ```text
//!   A1B2-C3D4-…-Z9Y8@https://litecter-sync.alice.workers.dev
//! ```
//!
//! Not encoded, on purpose. A base64 blob would be shorter and completely
//! opaque; this is a string someone can look at and see what they are about to
//! paste — which matters, because the part before the `@` is the only copy of
//! the key that can decrypt their backup.

use anyhow::{Context, Result, bail};
use url::Url;

use super::key::SyncKey;

/// A parsed pairing string. The endpoint is optional so that a bare sync key —
/// what earlier versions handed out, and what someone will inevitably paste —
/// still works instead of erroring.
#[derive(Debug)]
pub struct Link {
    pub key: SyncKey,
    pub endpoint: Option<String>,
}

pub fn encode(key: &SyncKey, endpoint: &str) -> String {
    format!("{}@{}", key.encode(), endpoint)
}

pub fn parse(raw: &str) -> Result<Link> {
    let raw = raw.trim();
    let (key_part, endpoint_part) = match raw.split_once('@') {
        Some((k, e)) => (k, Some(e)),
        None => (raw, None),
    };

    let key = SyncKey::decode(key_part).context("that does not look like a sync key")?;
    let endpoint = endpoint_part.map(normalize_endpoint).transpose()?;
    Ok(Link { key, endpoint })
}

/// Turn whatever the user pasted into the origin the client should call.
///
/// Generous about the shape because every one of these is something a person
/// will actually paste: a bare hostname, a trailing slash, or the full API URL
/// copied out of this README.
pub fn normalize_endpoint(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        bail!("the backend address is empty");
    }

    // `wrangler deploy` prints a bare hostname often enough to be worth
    // handling. Assume TLS; nobody should be syncing over plaintext.
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };

    let url = Url::parse(&with_scheme)
        .with_context(|| format!("`{raw}` is not a valid address"))?;

    match url.scheme() {
        "https" => {}
        // Only for `wrangler dev`, which serves on localhost without TLS.
        "http" if matches!(url.host_str(), Some("localhost" | "127.0.0.1")) => {}
        "http" => bail!("the backend address must use https"),
        other => bail!("`{other}://` is not an address Litecter can sync to"),
    }

    let host = url.host_str().context("the address has no host")?;
    if !url.username().is_empty() || url.password().is_some() {
        bail!("the backend address must not contain a username or password");
    }

    let mut origin = format!("{}://{host}", url.scheme());
    if let Some(port) = url.port() {
        origin.push_str(&format!(":{port}"));
    }

    // Someone who copies the URL out of the worker README arrives here with an
    // API route stuck on the end. Strip it rather than making them notice.
    let path = url.path().trim_end_matches('/');
    let path = ["/v1/blob", "/v1/meta", "/v1/health"]
        .iter()
        .fold(path, |p, route| p.strip_suffix(route).unwrap_or(p));
    origin.push_str(path);

    Ok(origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SyncKey {
        SyncKey::generate().unwrap()
    }

    #[test]
    fn a_pairing_string_round_trips() {
        let k = key();
        let code = encode(&k, "https://litecter-sync.alice.workers.dev");
        let parsed = parse(&code).unwrap();
        assert_eq!(parsed.key.encode(), k.encode());
        assert_eq!(parsed.endpoint.as_deref(), Some("https://litecter-sync.alice.workers.dev"));
    }

    #[test]
    fn a_bare_key_still_works() {
        // What the previous version of this feature handed out. It should adopt
        // the key and leave the endpoint to be filled in, not reject the paste.
        let k = key();
        let parsed = parse(&k.encode()).unwrap();
        assert_eq!(parsed.key.encode(), k.encode());
        assert!(parsed.endpoint.is_none());
    }

    #[test]
    fn endpoints_are_normalised_to_an_origin() {
        for (input, want) in [
            ("https://sync.example.com/", "https://sync.example.com"),
            ("sync.example.com", "https://sync.example.com"),
            ("  https://sync.example.com  ", "https://sync.example.com"),
            ("https://sync.example.com/v1/blob", "https://sync.example.com"),
            ("https://sync.example.com/v1/health/", "https://sync.example.com"),
            ("http://localhost:8787", "http://localhost:8787"),
        ] {
            assert_eq!(normalize_endpoint(input).unwrap(), want, "input: {input}");
        }
    }

    #[test]
    fn a_worker_behind_a_path_keeps_its_path() {
        assert_eq!(
            normalize_endpoint("https://example.com/litecter/").unwrap(),
            "https://example.com/litecter",
        );
    }

    #[test]
    fn plaintext_and_odd_schemes_are_refused() {
        assert!(normalize_endpoint("http://sync.example.com").is_err());
        assert!(normalize_endpoint("ftp://sync.example.com").is_err());
        assert!(normalize_endpoint("").is_err());
    }

    #[test]
    fn credentials_in_the_url_are_refused() {
        // `@` is the pairing separator, so a URL carrying userinfo would split
        // in the wrong place and hand back a mangled key.
        assert!(normalize_endpoint("https://user:pw@sync.example.com").is_err());
    }

    #[test]
    fn a_mangled_key_names_the_key_not_the_url() {
        let e = parse("not-a-key@https://sync.example.com").unwrap_err();
        assert!(format!("{e:#}").contains("sync key"), "got: {e:#}");
    }
}
