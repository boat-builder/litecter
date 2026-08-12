use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::Page;
use futures::StreamExt;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

const NAV_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT_SELECTOR_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_SETTLE_MS: u64 = 1_500;

/// One headless Chromium with a single reused tab.
///
/// Lifecycle is wake → check → sleep: callers launch this only when checks are
/// due and `shutdown()` it as soon as the batch drains. At rest no browser
/// process exists.
pub struct Renderer {
    browser: Browser,
    page: Page,
    handler_loop: JoinHandle<()>,
}

pub struct RenderedPage {
    pub title: Option<String>,
    pub text: String,
}

/// Find a working browser binary. `LITECTER_CHROME` wins; otherwise the first
/// well-known install that actually exists (chromiumoxide's own detection can
/// pick stale wrappers, e.g. a Homebrew cask whose app was removed).
fn find_browser() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("LITECTER_CHROME") {
        return Some(p.into());
    }
    const MAC_APPS: &[&str] = &[
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    ];
    const LINUX_BINS: &[&str] = &[
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/snap/bin/chromium",
    ];
    MAC_APPS
        .iter()
        .chain(LINUX_BINS)
        .map(std::path::Path::new)
        .find(|p| p.is_file())
        .map(Into::into)
}

impl Renderer {
    pub async fn launch() -> Result<Self> {
        let mut builder = BrowserConfig::builder()
            .new_headless_mode()
            .window_size(1280, 1024);
        if let Some(exe) = find_browser() {
            builder = builder.chrome_executable(exe);
        }
        let config = builder
            .build()
            .map_err(|e| anyhow!("browser config: {e}"))?;
        let (browser, mut handler) = Browser::launch(config)
            .await
            .context("failed to launch Chrome/Chromium — is it installed?")?;
        // The CDP event pump; the browser connection is dead without it.
        let handler_loop = tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if event.is_err() {
                    break;
                }
            }
        });
        let page = browser.new_page("about:blank").await?;
        Ok(Self { browser, page, handler_loop })
    }

    /// Navigate, let the page settle, and return its rendered visible text.
    pub async fn render(
        &self,
        url: &str,
        selector: Option<&str>,
        wait_selector: Option<&str>,
        settle_ms: Option<u64>,
    ) -> Result<RenderedPage> {
        let nav = timeout(NAV_TIMEOUT, async {
            self.page.goto(url).await?;
            let resp = self.page.wait_for_navigation_response().await?;
            Ok::<_, anyhow::Error>(resp)
        })
        .await
        .map_err(|_| anyhow!("navigation timed out after {}s", NAV_TIMEOUT.as_secs()))??;

        if let Some(req) = nav.as_deref() {
            if let Some(fail) = &req.failure_text {
                return Err(anyhow!("navigation failed: {fail}"));
            }
            if let Some(resp) = &req.response {
                if resp.status >= 400 {
                    return Err(anyhow!("HTTP {}", resp.status));
                }
            }
        }

        if let Some(sel) = wait_selector {
            let probe = format!("!!document.querySelector({})", serde_json::to_string(sel)?);
            let deadline = tokio::time::Instant::now() + WAIT_SELECTOR_TIMEOUT;
            loop {
                let found: bool = self
                    .page
                    .evaluate(probe.as_str())
                    .await?
                    .into_value()
                    .unwrap_or(false);
                if found {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(anyhow!(
                        "wait selector {sel:?} did not appear within {}s",
                        WAIT_SELECTOR_TIMEOUT.as_secs()
                    ));
                }
                sleep(Duration::from_millis(250)).await;
            }
        }

        sleep(Duration::from_millis(settle_ms.unwrap_or(DEFAULT_SETTLE_MS))).await;

        // innerText = what a human sees: JS-rendered, hidden elements excluded.
        // Returns null (→ error below) when a configured selector matches nothing,
        // so a vanished selector can't masquerade as "the whole page emptied".
        let extract = format!(
            "(() => {{ const sel = {sel}; if (sel) {{ const el = document.querySelector(sel); return el ? el.innerText : null; }} return document.body ? document.body.innerText : ''; }})()",
            sel = serde_json::to_string(&selector)?
        );
        let text: Option<String> = self
            .page
            .evaluate(extract.as_str())
            .await?
            .into_value()
            .context("extracting page text")?;
        let Some(text) = text else {
            return Err(anyhow!("selector {:?} not found on page", selector.unwrap_or_default()));
        };

        let title: Option<String> = self
            .page
            .evaluate("document.title")
            .await
            .ok()
            .and_then(|v| v.into_value::<String>().ok())
            .filter(|t| !t.trim().is_empty());

        // Blank the tab so the last page's JS can't keep running between checks.
        let _ = self.page.goto("about:blank").await;

        Ok(RenderedPage { title, text })
    }

    pub async fn shutdown(mut self) {
        let _ = self.browser.close().await;
        let _ = self.browser.wait().await;
        self.handler_loop.abort();
    }
}
