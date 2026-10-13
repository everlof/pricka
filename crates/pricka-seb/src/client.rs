use std::collections::HashMap;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::page::Page;
use futures::StreamExt;
use pricka_core::{PrickaError, ProxyConfig};
use serde::Deserialize;
use tokio::task::JoinHandle;
use tracing::debug;

/// Holds the Chrome browser instance, active page, and CDP handler task.
pub struct BrowserSession {
    browser: Browser,
    pub page: Page,
    handler: JoinHandle<()>,
}

/// Response from a fetch() call executed inside Chrome.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct FetchResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl BrowserSession {
    /// Launch Chrome with optional proxy and headless mode.
    pub async fn launch(proxy: &ProxyConfig, headless: bool) -> Result<Self, PrickaError> {
        let mut builder = BrowserConfig::builder();
        if let Some(path) = std::env::var_os("PRICKA_CHROME_BIN") {
            builder = builder.chrome_executable(path);
        }
        if std::env::var("PRICKA_CHROME_NO_SANDBOX").as_deref() == Ok("1") {
            builder = builder.no_sandbox();
        }
        if !headless {
            builder = builder.with_head();
        }

        match proxy {
            ProxyConfig::None => {}
            ProxyConfig::Socks5(url) | ProxyConfig::Http(url) => {
                builder = builder.arg(format!("--proxy-server={url}"));
            }
        }

        let config = builder
            .build()
            .map_err(|e| PrickaError::Browser(format!("BrowserConfig build: {e}")))?;

        let (browser, mut handler) = Browser::launch(config)
            .await
            .map_err(|e| PrickaError::Browser(format!("Browser launch: {e}")))?;

        let handle = tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if h.is_err() {
                    break;
                }
            }
        });

        let page = browser
            .new_page("about:blank")
            .await
            .map_err(|e| PrickaError::Browser(format!("New page: {e}")))?;

        debug!("Chrome browser launched for SEB");

        Ok(Self {
            browser,
            page,
            handler: handle,
        })
    }

    /// Close the browser and stop the handler task.
    pub async fn close(mut self) {
        let _ = self.browser.close().await;
        self.handler.abort();
    }
}

/// Execute a GET request inside Chrome via `fetch()`.
pub async fn browser_get(
    page: &Page,
    url: &str,
    headers: &[(&str, &str)],
) -> Result<FetchResponse, PrickaError> {
    let headers_js = build_headers_js(headers);

    let js = format!(
        r#"
        (async () => {{
            const r = await fetch({url}, {{
                method: 'GET',
                headers: {headers_js},
                credentials: 'include'
            }});
            const body = await r.text();
            const hdrs = {{}};
            r.headers.forEach((v, k) => {{ hdrs[k] = v; }});
            return JSON.stringify({{ status: r.status, headers: hdrs, body: body }});
        }})()
        "#,
        url = serde_json::to_string(url).unwrap(),
        headers_js = headers_js,
    );

    let result: String = page
        .evaluate(js)
        .await
        .map_err(|e| PrickaError::Browser(format!("evaluate GET {url}: {e}")))?
        .into_value()
        .map_err(|e| PrickaError::Browser(format!("deserialize GET {url}: {e}")))?;

    serde_json::from_str(&result)
        .map_err(|e| PrickaError::Browser(format!("parse FetchResponse GET {url}: {e}")))
}

/// Execute a POST request inside Chrome via `fetch()`.
pub async fn browser_post(
    page: &Page,
    url: &str,
    headers: &[(&str, &str)],
    body: &serde_json::Value,
) -> Result<FetchResponse, PrickaError> {
    let headers_js = build_headers_js(headers);
    let body_json = serde_json::to_string(body).unwrap();

    let js = format!(
        r#"
        (async () => {{
            const r = await fetch({url}, {{
                method: 'POST',
                headers: {headers_js},
                credentials: 'include',
                body: {body}
            }});
            const respBody = await r.text();
            const hdrs = {{}};
            r.headers.forEach((v, k) => {{ hdrs[k] = v; }});
            return JSON.stringify({{ status: r.status, headers: hdrs, body: respBody }});
        }})()
        "#,
        url = serde_json::to_string(url).unwrap(),
        headers_js = headers_js,
        body = serde_json::to_string(&body_json).unwrap(),
    );

    let result: String = page
        .evaluate(js)
        .await
        .map_err(|e| PrickaError::Browser(format!("evaluate POST {url}: {e}")))?
        .into_value()
        .map_err(|e| PrickaError::Browser(format!("deserialize POST {url}: {e}")))?;

    serde_json::from_str(&result)
        .map_err(|e| PrickaError::Browser(format!("parse FetchResponse POST {url}: {e}")))
}

/// Build a JavaScript object literal for headers.
fn build_headers_js(headers: &[(&str, &str)]) -> String {
    let entries: Vec<String> = headers
        .iter()
        .map(|(k, v)| {
            format!(
                "{}: {}",
                serde_json::to_string(k).unwrap(),
                serde_json::to_string(v).unwrap()
            )
        })
        .collect();
    format!("{{{}}}", entries.join(", "))
}
