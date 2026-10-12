use crate::client::{browser_get, browser_post};
use chromiumoxide::page::Page;
use pricka_core::{AuthPresenter, PrickaError};
use serde::Deserialize;
use tokio::time::sleep;
use tracing::{debug, info, trace, warn};

const BASE_URL: &str = "https://secure.handelsbanken.se";

/// Common headers used by Handelsbanken's login endpoints.
fn login_headers() -> Vec<(&'static str, &'static str)> {
    vec![
        ("accept", "application/json, application/problem+json"),
        ("accept-language", "sv"),
        ("x-requested-with", "fetch"),
    ]
}

/// Response from the BankID init endpoint.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitResponse {
    qr_start_token: String,
    initial_sleep_time: u64,
    #[serde(rename = "_links")]
    links: InitLinks,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct InitLinks {
    authenticate: LinkHref,
    cancel: LinkHref,
}

#[derive(Debug, Deserialize)]
struct LinkHref {
    href: String,
}

/// Response from polling the authenticate endpoint.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthenticateResponse {
    result: String,
    iteration_sleep_time: u64,
    /// Updated QR token — present when result is NO_CLIENT_STARTED.
    qr_start_token: Option<String>,
    #[serde(rename = "_links")]
    links: Option<AuthenticateLinks>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct AuthenticateLinks {
    application_entry_point: Option<LinkHref>,
}

/// Bootstrap response body — contains session tokens needed for all API calls.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapResponse {
    feature_toggles: Option<FeatureToggles>,
    user_characteristics: Option<UserCharacteristics>,
}

#[derive(Debug, Deserialize)]
struct FeatureToggles {
    key: String,
}

#[derive(Debug, Deserialize)]
struct UserCharacteristics {
    key: String,
}

/// Session tokens extracted from the bootstrap response.
/// These must be sent as headers on all subsequent API calls.
#[derive(Debug, Clone)]
pub struct SessionTokens {
    /// From bootstrap response header `x-shb-request-synchronized-token`.
    pub synchronized_token: String,
    /// From bootstrap response body `userCharacteristics.key`.
    pub characteristics_key: String,
    /// From bootstrap response body `featureToggles.key`.
    pub feature_toggles_key: String,
}

/// Run the full BankID QR login flow for Handelsbanken.
///
/// Returns the session tokens needed for all subsequent API calls.
pub async fn login(
    page: &Page,
    personnummer: &str,
    presenter: &dyn AuthPresenter,
) -> pricka_core::Result<SessionTokens> {
    let headers = login_headers();

    // Step 1: Load login page to establish cookies (real browser navigation)
    debug!("Loading login page...");
    presenter.present_status("Loading login page...");
    page.goto(format!("{BASE_URL}/logon/se/priv/sv/"))
        .await
        .map_err(|e| PrickaError::Browser(format!("goto login page: {e}")))?;
    trace!("Login page loaded");

    // Step 2: Get entrypoints to find MBIDQR init URL
    debug!("Fetching login entrypoints...");
    let resp = browser_get(
        page,
        &format!("{BASE_URL}/mlurg/entrypoints/web/sepriv/1.0"),
        &headers,
    )
    .await?;
    trace!(status = resp.status, "Entrypoints response");

    let entrypoints: serde_json::Value =
        serde_json::from_str(&resp.body).map_err(|e| PrickaError::Parse(e.to_string()))?;

    let init_url = entrypoints["entrypoints"]
        .as_array()
        .and_then(|eps| {
            eps.iter()
                .find(|ep| ep["logonTp"].as_str() == Some("MBIDQR"))
        })
        .and_then(|ep| ep["_links"]["init_1.0"]["href"].as_str())
        .ok_or_else(|| PrickaError::Parse("MBIDQR entrypoint not found".into()))?
        .to_string();

    debug!("Found MBIDQR init endpoint");

    // Step 3: POST init with personnummer
    presenter.present_status("Initiating BankID...");
    let init_body = serde_json::json!({
        "personalIdTp": 1,
        "personalId": personnummer,
        "bidDevice": "other",
    });

    let mut post_headers = headers.clone();
    post_headers.push(("content-type", "application/json;charset=UTF-8"));

    let resp = browser_post(page, &init_url, &post_headers, &init_body).await?;
    trace!(status = resp.status, "Init response");

    let init_resp: InitResponse = serde_json::from_str(&resp.body)
        .map_err(|e| PrickaError::Parse(format!("Init parse: {e}")))?;

    debug!(
        qr_token_len = init_resp.qr_start_token.len(),
        initial_sleep = init_resp.initial_sleep_time,
        "BankID init successful"
    );

    let authenticate_url = &init_resp.links.authenticate.href;

    // Display initial QR code
    presenter.present_qr(&init_resp.qr_start_token);
    presenter.present_status("Scanna QR-koden med BankID-appen...");

    // Initial sleep before first poll
    sleep(std::time::Duration::from_millis(
        init_resp.initial_sleep_time,
    ))
    .await;

    // Step 4: Poll authenticate endpoint
    let mut entry_point_url: Option<String> = None;

    for attempt in 0..60 {
        let resp = browser_post(
            page,
            authenticate_url,
            &post_headers,
            &serde_json::json!({}),
        )
        .await?;
        trace!(status = resp.status, attempt, "Auth poll response");

        let auth_resp: AuthenticateResponse = serde_json::from_str(&resp.body)
            .map_err(|e| PrickaError::Parse(format!("Auth parse: {e}")))?;

        debug!(
            attempt,
            result = &auth_resp.result,
            sleep_ms = auth_resp.iteration_sleep_time,
            has_qr = auth_resp.qr_start_token.is_some(),
            "Auth poll"
        );

        match auth_resp.result.as_str() {
            "NO_CLIENT_STARTED" => {
                if let Some(ref qr_token) = auth_resp.qr_start_token {
                    presenter.present_qr(qr_token);
                }
            }
            "IN_PROGRESS" => {
                presenter.present_status("Verifierar i BankID-appen...");
            }
            "AUTHORIZED" => {
                info!("BankID authentication successful");
                presenter.present_status("Inloggning lyckades!");
                entry_point_url = auth_resp
                    .links
                    .and_then(|l| l.application_entry_point)
                    .map(|l| l.href);
                debug!("Got entry point URL");
                break;
            }
            other => {
                warn!(result = other, "BankID authentication failed");
                return Err(PrickaError::AuthFailed(format!(
                    "Unexpected result: {other}"
                )));
            }
        }

        sleep(std::time::Duration::from_millis(
            auth_resp.iteration_sleep_time,
        ))
        .await;
    }

    let entry_url = entry_point_url.ok_or(PrickaError::AuthTimeout)?;

    // Step 5: Navigate to the entry point to establish the authenticated session
    debug!("Navigating to authenticated session");
    page.goto(&entry_url)
        .await
        .map_err(|e| PrickaError::Browser(format!("goto entry point: {e}")))?;
    trace!("Entry point loaded");

    // Step 6: Bootstrap call — this returns the session tokens we need
    debug!("Calling bootstrap endpoint");
    let resp = browser_post(
        page,
        &format!("{BASE_URL}/rsefa/ryia/bu/bootstrap/se/v2/private"),
        &headers,
        &serde_json::json!({}),
    )
    .await?;

    trace!(status = resp.status, "Bootstrap response");

    // Extract synchronized token from response header
    let synchronized_token = resp
        .headers
        .get("x-shb-request-synchronized-token")
        .cloned()
        .ok_or_else(|| {
            PrickaError::Parse(
                "Bootstrap response missing x-shb-request-synchronized-token header".into(),
            )
        })?;

    // Extract and set the cookie-synchronized-token as a cookie via document.cookie
    if let Some(cookie_token) = resp.headers.get("x-shb-cookie-synchronized-token") {
        let js = format!(
            "document.cookie = 'x-shb-cookie-synchronized-token={}; Path=/; Secure'",
            cookie_token
        );
        page.evaluate(js)
            .await
            .map_err(|e| PrickaError::Browser(format!("set cookie: {e}")))?;
        debug!("Set cookie-synchronized-token cookie");
    }

    debug!("Got synchronized token from bootstrap");

    let body = &resp.body;
    trace!(body_len = body.len(), "Bootstrap body received");

    let bootstrap: BootstrapResponse = serde_json::from_str(body)
        .map_err(|e| PrickaError::Parse(format!("Bootstrap parse: {e}")))?;

    let characteristics_key = bootstrap
        .user_characteristics
        .map(|uc| uc.key)
        .ok_or_else(|| {
            PrickaError::Parse("Bootstrap response missing userCharacteristics.key".into())
        })?;

    let feature_toggles_key = bootstrap.feature_toggles.map(|ft| ft.key).ok_or_else(|| {
        PrickaError::Parse("Bootstrap response missing featureToggles.key".into())
    })?;

    debug!("Got session tokens from bootstrap");

    // Step 7: Fire the same preliminary requests the SPA makes.
    debug!("Warming up service sessions...");
    let warmup_urls = [
        format!("{BASE_URL}/tron/priv/ui/configurations/v1/sitemap/sitemap?targetGroups=PRIVATE"),
        format!("{BASE_URL}/tron/priv/ui/navigations/v2/menu/menu2?targetGroups=PRIVATE"),
        format!("{BASE_URL}/tron/priv/ui/navigations/v2/menu/nav-myprofile?targetGroups=PRIVATE"),
        format!("{BASE_URL}/tron/priv/info/alerts/v1/alert/31-17530?targetGroups=PRIVATE"),
        format!("{BASE_URL}/tron/priv/info/help/v1/view/31-18194-64?targetGroups=PRIVATE"),
        format!("{BASE_URL}/pargb/parg/bu/parts/v2/branches/6758"),
        format!("{BASE_URL}/semeresourceexternal/seme/bu/securemessage/v2/numberOfUnread"),
        format!("{BASE_URL}/rseoa/docm/bu/individual/documents/v1/correspondence/numberOfUnread"),
        format!("{BASE_URL}/rsela/admn/ui/settings/v1/favorites"),
        format!("{BASE_URL}/rscta/card/v2/cards"),
        format!("{BASE_URL}/rsefa/yoda/bu/parts/customerGreetings/v1"),
    ];

    let api_headers: Vec<(&str, &str)> = vec![
        ("accept", "application/json, application/problem+json"),
        ("accept-language", "sv"),
        ("x-requested-with", "fetch"),
        ("content-type", "application/json;charset=UTF-8"),
        ("referer", "https://secure.handelsbanken.se/se/private/sv/"),
        ("x-shb-synchronized-token", &synchronized_token),
        ("x-shb-characteristicskey", &characteristics_key),
        ("x-shb-featuretoggleskey", &feature_toggles_key),
    ];

    // Fire all warmup requests concurrently
    let warmup_futures: Vec<_> = warmup_urls
        .iter()
        .map(|url| {
            let headers = api_headers.clone();
            let url = url.clone();
            async move {
                let resp = browser_get(page, &url, &headers).await;
                match &resp {
                    Ok(r) => trace!(url, status = r.status, "Warmup request"),
                    Err(e) => trace!(url, error = %e, "Warmup request failed"),
                }
                resp.map(|r| r.status)
            }
        })
        .collect();

    let results = futures::future::join_all(warmup_futures).await;
    for (url, result) in warmup_urls.iter().zip(results.iter()) {
        match result {
            Ok(status) => debug!(url, status, "Warmup"),
            Err(e) => warn!(url, error = %e, "Warmup failed"),
        }
    }

    info!("Login flow complete, session established");

    Ok(SessionTokens {
        synchronized_token,
        characteristics_key,
        feature_toggles_key,
    })
}
