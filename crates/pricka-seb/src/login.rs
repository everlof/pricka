use chromiumoxide::page::Page;
use pricka_core::{AuthPresenter, PrickaError, Result};
use serde::Deserialize;
use tracing::{debug, trace};

use crate::client::{browser_get, browser_post};

const CLIENT_ID: &str = "ccsext";
const AUTH_BASE: &str = "https://id.seb.se";
const APP_BASE: &str = "https://apps.seb.se";

/// OAuth parameters extracted from the login page URL.
struct OAuthParams {
    state: String,
    code_challenge: String,
    redirect_uri: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BankIdInitResponse {
    order_id: String,
    qr_token: Option<String>,
    status: String,
}

#[derive(Debug, Deserialize)]
struct BankIdPollResponse {
    #[allow(dead_code)]
    order_id: String,
    qr_token: Option<String>,
    status: String,
    hint_code: Option<String>,
    gl_order_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GlAuthResponse {
    status: String,
}

/// Session info extracted after successful login.
pub struct SessionInfo {
    /// The user ID / client-id for API calls (10-digit personnummer from JWT)
    pub client_id: String,
    /// The company/organization ID.
    pub organization_id: String,
}

/// Run the full SEB Företag login flow:
/// 1. Navigate to apps.seb.se → redirects to id.seb.se login page
/// 2. Extract OAuth params from URL
/// 3. Initiate BankID QR auth
/// 4. Poll BankID status (display QR)
/// 5. Poll GL auth completion
/// 6. Navigate to OAuth authorize → redirects back to apps.seb.se with session
/// 7. Extract organization ID from authenticated page
pub async fn login(page: &Page, presenter: &dyn AuthPresenter) -> Result<SessionInfo> {
    // Step 1: Navigate to the app - this triggers the OAuth redirect chain
    // and lands on the id.seb.se login page
    debug!("Navigating to SEB Business Arena...");
    page.goto(format!("{APP_BASE}/ccs/home"))
        .await
        .map_err(|e| PrickaError::Browser(format!("goto /ccs/home: {e}")))?;

    // Wait for the login page to load (Angular SPA on id.seb.se)
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // Step 2: Extract OAuth params from the current URL
    let current_url = page
        .url()
        .await
        .map_err(|e| PrickaError::Browser(format!("get URL: {e}")))?
        .ok_or_else(|| PrickaError::Browser("No URL available".into()))?;

    debug!("Login page loaded");

    let oauth_params = extract_oauth_params(&current_url)?;
    debug!("Extracted OAuth state and code_challenge");

    // Step 3: Initiate BankID QR authentication
    presenter.present_status("Initierar BankID...");

    let init_body = serde_json::json!({
        "client_id": CLIENT_ID,
        "start_mode": "QR",
        "ui_locales": "sv"
    });

    let resp = browser_post(
        page,
        &format!("{AUTH_BASE}/auth/bid/v4/authentications"),
        &[
            ("accept", "application/json, text/plain, */*"),
            ("content-type", "application/json"),
        ],
        &init_body,
    )
    .await?;

    if resp.status != 200 {
        return Err(PrickaError::AuthFailed(format!(
            "BankID init returned {}",
            resp.status
        )));
    }

    let init: BankIdInitResponse = serde_json::from_str(&resp.body)
        .map_err(|e| PrickaError::Parse(format!("BankID init response: {e}")))?;

    debug!("BankID order created");

    if let Some(qr) = &init.qr_token {
        presenter.present_qr(qr);
    }

    // Step 4: Poll BankID status
    let gl_order_id = poll_bankid(page, &init.order_id, presenter).await?;
    debug!("BankID complete");

    // Step 5: Poll GL auth
    presenter.present_status("Slutför inloggning...");
    poll_gl_auth(page, &gl_order_id).await?;
    debug!("GL auth complete");

    // Step 6: Navigate to OAuth authorize endpoint
    // This triggers the code exchange and redirects back to apps.seb.se
    let mut authorize_url =
        url::Url::parse(&format!("{AUTH_BASE}/mga/sps/oauth/oauth20/authorize")).unwrap();
    {
        let mut q = authorize_url.query_pairs_mut();
        q.append_pair("client_id", CLIENT_ID);
        q.append_pair("redirect_path", "/ccs/home");
        // SEB prepends "sv__" to state per prepend_lang_to_state config
        q.append_pair("state", &format!("sv__{}", oauth_params.state));
        q.append_pair("redirect_uri", &oauth_params.redirect_uri);
        q.append_pair("response_type", "code");
        q.append_pair("code_challenge", &oauth_params.code_challenge);
        q.append_pair("code_challenge_method", "S256");
    }

    debug!("Navigating to OAuth authorize...");
    page.goto(authorize_url.as_str())
        .await
        .map_err(|e| PrickaError::Browser(format!("goto authorize: {e}")))?;

    // Wait for redirects to complete - we should land on apps.seb.se/ccs/home
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let final_url = page
        .url()
        .await
        .map_err(|e| PrickaError::Browser(format!("get final URL: {e}")))?
        .unwrap_or_default();

    debug!("OAuth redirect completed");

    if !final_url.contains("apps.seb.se") {
        return Err(PrickaError::AuthFailed(
            "Expected redirect to apps.seb.se".into(),
        ));
    }

    // Step 7: Extract user_id and organization ID from window.$$Arena
    // Wait a moment for the page JS to initialize
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let arena_info: String = page
        .evaluate(
            r#"
            (function() {
                if (window.$$Arena && window.$$Arena.principalDetails && window.$$Arena.principalDetails.length > 0) {
                    return JSON.stringify({
                        user_id: window.$$Arena.user_id || "",
                        organization_id: window.$$Arena.principalDetails[0].id || ""
                    });
                }
                return "";
            })()
            "#,
        )
        .await
        .map_err(|e| PrickaError::Browser(format!("extract arena info: {e}")))?
        .into_value()
        .map_err(|e| PrickaError::Browser(format!("deserialize arena info: {e}")))?;

    if arena_info.is_empty() {
        return Err(PrickaError::AuthFailed(
            "Could not extract session info from page".into(),
        ));
    }

    #[derive(Deserialize)]
    struct ArenaInfo {
        user_id: String,
        organization_id: String,
    }

    let info: ArenaInfo = serde_json::from_str(&arena_info)
        .map_err(|e| PrickaError::Parse(format!("arena info: {e}")))?;

    debug!("SEB session identifiers loaded");
    presenter.present_status("Inloggad!");

    Ok(SessionInfo {
        client_id: info.user_id,
        organization_id: info.organization_id,
    })
}

/// Poll BankID status until COMPLETE, returning the gl_order_id.
async fn poll_bankid(page: &Page, order_id: &str, presenter: &dyn AuthPresenter) -> Result<String> {
    let poll_url = format!("{AUTH_BASE}/auth/bid/v4/authentications/{order_id}");
    let mut last_hint = String::new();

    for attempt in 0..300 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let resp = browser_get(
            page,
            &poll_url,
            &[("accept", "application/json, text/plain, */*")],
        )
        .await?;

        if resp.status != 200 {
            return Err(PrickaError::AuthFailed(format!(
                "BankID poll returned {}",
                resp.status
            )));
        }

        let poll: BankIdPollResponse = serde_json::from_str(&resp.body)
            .map_err(|e| PrickaError::Parse(format!("BankID poll response: {e}")))?;

        trace!(
            "BankID poll #{attempt}: status={}, hint={:?}",
            poll.status, poll.hint_code
        );

        // Update QR code if available (server provides animated QR)
        if let Some(qr) = &poll.qr_token {
            presenter.present_qr(qr);
        }

        // Update status on hint change
        let hint = poll.hint_code.as_deref().unwrap_or("");
        if hint != last_hint {
            let status_msg = match hint {
                "OUTSTANDING_TRANSACTION" => "Väntar på att du skannar QR-koden...",
                "USER_SIGN" => "Skriv in din säkerhetskod i BankID-appen...",
                "STARTED" => "BankID-appen öppnad...",
                _ => hint,
            };
            presenter.present_status(status_msg);
            last_hint = hint.to_string();
        }

        match poll.status.as_str() {
            "COMPLETE" => {
                return poll.gl_order_id.ok_or_else(|| {
                    PrickaError::AuthFailed("BankID COMPLETE but no gl_order_id".into())
                });
            }
            "PENDING" => continue,
            "FAILED" => {
                let reason = poll.hint_code.unwrap_or("unknown".into());
                return Err(PrickaError::AuthFailed(format!("BankID failed: {reason}")));
            }
            other => {
                return Err(PrickaError::AuthFailed(format!(
                    "Unexpected BankID status: {other}"
                )));
            }
        }
    }

    Err(PrickaError::AuthTimeout)
}

/// Poll GL auth until COMPLETE.
async fn poll_gl_auth(page: &Page, gl_order_id: &str) -> Result<()> {
    let poll_url = format!("{AUTH_BASE}/auth/gl/v1/authentications/{gl_order_id}");

    for attempt in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let resp = browser_post(
            page,
            &poll_url,
            &[
                ("accept", "application/json, text/plain, */*"),
                ("content-type", "application/json"),
            ],
            &serde_json::json!({}),
        )
        .await?;

        if resp.status != 200 {
            return Err(PrickaError::AuthFailed(format!(
                "GL auth poll returned {}",
                resp.status
            )));
        }

        let gl: GlAuthResponse = serde_json::from_str(&resp.body)
            .map_err(|e| PrickaError::Parse(format!("GL auth response: {e}")))?;

        trace!("GL auth poll #{attempt}: status={}", gl.status);

        match gl.status.as_str() {
            "COMPLETE" => return Ok(()),
            "PENDING" => continue,
            other => {
                return Err(PrickaError::AuthFailed(format!(
                    "Unexpected GL auth status: {other}"
                )));
            }
        }
    }

    Err(PrickaError::AuthTimeout)
}

/// Extract OAuth params (state, code_challenge, redirect_uri) from the login page URL.
fn extract_oauth_params(url: &str) -> Result<OAuthParams> {
    let parsed =
        url::Url::parse(url).map_err(|e| PrickaError::Parse(format!("Invalid login URL: {e}")))?;

    let params: std::collections::HashMap<String, String> = parsed
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    let state = params
        .get("state")
        .ok_or_else(|| PrickaError::Parse("Missing 'state' in login URL".into()))?
        .clone();

    let code_challenge = params
        .get("code_challenge")
        .ok_or_else(|| PrickaError::Parse("Missing 'code_challenge' in login URL".into()))?
        .clone();

    let redirect_uri = params
        .get("redirect_uri")
        .ok_or_else(|| PrickaError::Parse("Missing 'redirect_uri' in login URL".into()))?
        .clone();

    Ok(OAuthParams {
        state,
        code_challenge,
        redirect_uri,
    })
}
