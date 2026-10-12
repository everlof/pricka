//! Handelsbanken company BankID login. Session values stay in the browser or memory.

use crate::client::{FetchResponse, browser_get, browser_post};
use chromiumoxide::cdp::browser_protocol::network::EventRequestWillBeSent;
use futures::StreamExt;
use pricka_core::{AuthPresenter, PrickaError, Result};
use serde::{Deserialize, de::DeserializeOwned};
use std::{collections::HashMap, time::Duration};

const ORIGIN: &str = "https://secure.handelsbanken.se";
const HEADERS: &[(&str, &str)] = &[
    ("accept", "application/json, application/problem+json"),
    ("accept-language", "sv"),
    ("x-requested-with", "fetch"),
    ("content-type", "application/json;charset=UTF-8"),
];

#[derive(Deserialize)]
struct Link {
    href: String,
}

#[derive(Deserialize)]
struct Links {
    #[serde(rename = "_links")]
    links: HashMap<String, Link>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entrypoint {
    logon_tp: String,
    #[serde(flatten)]
    links: Links,
}

#[derive(Deserialize)]
struct Entrypoints {
    entrypoints: Vec<Entrypoint>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Init {
    qr_start_token: Option<String>,
    initial_sleep_time: u64,
    #[serde(flatten)]
    links: Links,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Challenge {
    qr_start_token: String,
    initial_sleep_time: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Poll {
    result: String,
    #[serde(default)]
    iteration_sleep_time: u64,
    qr_start_token: Option<String>,
    #[serde(rename = "_links", default)]
    links: HashMap<String, Link>,
}

enum PollAction {
    Continue,
    Refresh,
    Authenticated,
}

struct LoginFailure {
    stage: &'static str,
    reason: &'static str,
}

fn at<T>(stage: &'static str, result: Result<T>) -> std::result::Result<T, LoginFailure> {
    result.map_err(|error| LoginFailure {
        stage,
        reason: match error {
            PrickaError::AuthTimeout => "timed_out",
            PrickaError::Parse(_) => "unexpected_response",
            PrickaError::AuthFailed(_) => "bank_rejected",
            _ => "request_failed",
        },
    })
}

fn poll_action(result: &str) -> std::result::Result<PollAction, LoginFailure> {
    match result {
        "NO_CLIENT_STARTED" | "NO_ID_FOUND" | "IN_PROGRESS" => Ok(PollAction::Continue),
        "QR_REFRESH" => Ok(PollAction::Refresh),
        "AUTHORIZED" | "AUTHENTICATED" => Ok(PollAction::Authenticated),
        "USER_CANCEL" => Err(LoginFailure {
            stage: "poll",
            reason: "user_cancelled",
        }),
        "QR_EXPIRED" => Err(LoginFailure {
            stage: "poll",
            reason: "qr_expired",
        }),
        "MUST_ACTIVATE" => Err(LoginFailure {
            stage: "poll",
            reason: "activation_required",
        }),
        "ERROR" | "CLIENT_START_FAILED" | "BISP_CLIENT_ERROR" => Err(LoginFailure {
            stage: "poll",
            reason: "bank_rejected",
        }),
        _ => Err(LoginFailure {
            stage: "poll",
            reason: "unknown_result",
        }),
    }
}

fn parse<T: DeserializeOwned>(response: FetchResponse) -> Result<T> {
    if !(200..300).contains(&response.status) {
        return Err(PrickaError::AuthFailed(format!(
            "Bank HTTP {}",
            response.status
        )));
    }
    // Response bodies and deserializer errors can contain customer values.
    serde_json::from_str(&response.body)
        .map_err(|_| PrickaError::Parse("Unexpected corporate login response".into()))
}

fn bank_url(link: &str) -> Result<String> {
    let base = url::Url::parse(ORIGIN).expect("fixed bank origin is valid");
    let url = base
        .join(link)
        .map_err(|_| PrickaError::Parse("Invalid bank link".into()))?;
    if url.origin() != base.origin() || !url.username().is_empty() || url.password().is_some() {
        return Err(PrickaError::Parse("Unexpected bank link origin".into()));
    }
    Ok(url.into())
}

fn link(links: &HashMap<String, Link>, name: &str) -> Result<String> {
    bank_url(
        &links
            .get(name)
            .ok_or_else(|| PrickaError::Parse("Missing bank link".into()))?
            .href,
    )
}

enum NextStep {
    CompanySelection,
    ApplicationEntry,
}

struct Handoff {
    next: NextStep,
    url: String,
}

fn next_step(links: &HashMap<String, Link>) -> Result<Handoff> {
    if links.contains_key("getMandates") {
        Ok(Handoff {
            next: NextStep::CompanySelection,
            url: link(links, "getMandates")?,
        })
    } else {
        Ok(Handoff {
            next: NextStep::ApplicationEntry,
            url: link(links, "application-entry-point")?,
        })
    }
}

fn bank_request_headers(value: &serde_json::Value) -> Vec<(&'static str, String)> {
    const NAMES: &[&str] = &[
        "x-shb-synchronized-token",
        "x-shb-characteristicskey",
        "x-shb-featuretoggleskey",
    ];
    let Some(map) = value.as_object() else {
        return Vec::new();
    };
    NAMES
        .iter()
        .filter_map(|name| {
            map.iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .and_then(|(_, value)| value.as_str())
                .map(|value| (*name, value.to_owned()))
        })
        .collect()
}

async fn poll_delay(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms.clamp(500, 5000))).await;
}

async fn entrypoint(page: &chromiumoxide::page::Page) -> Result<String> {
    page.goto(format!("{ORIGIN}/logon/se/corp/sv/"))
        .await
        .map_err(|_| PrickaError::Browser("Corporate login navigation failed".into()))?;
    let response: Entrypoints = parse(
        browser_get(
            page,
            &format!("{ORIGIN}/mlurg/entrypoints/web/secorp/1.0"),
            HEADERS,
        )
        .await?,
    )?;
    let endpoint = response
        .entrypoints
        .iter()
        .find(|entry| entry.logon_tp == "MBIDQR")
        .ok_or_else(|| PrickaError::Parse("Company QR entrypoint unavailable".into()))?;
    link(&endpoint.links.links, "init_1.0")
}

async fn authenticate_handoff(
    page: &chromiumoxide::page::Page,
    personnummer: &str,
    presenter: &dyn AuthPresenter,
) -> std::result::Result<Handoff, LoginFailure> {
    presenter.present_status("Connecting to Handelsbanken Företag…");
    let init_url = at("entrypoints", entrypoint(page).await)?;
    presenter.present_status("Starting BankID…");
    let init: Init = at(
        "init",
        parse(at(
            "init",
            browser_post(
                page,
                &init_url,
                HEADERS,
                &serde_json::json!({"personalIdTp":1,"personalId":personnummer,"bidDevice":"other"}),
            )
            .await,
        )?),
    )?;
    let mut links = init.links.links;
    let cancel = at(
        "init",
        links
            .get("cancel")
            .map(|value| bank_url(&value.href))
            .transpose(),
    )?;

    let auth_result = async {
        if let Some(qr) = &init.qr_start_token {
            presenter.present_qr(qr);
        }
        presenter.present_status("Scan with BankID and approve the Handelsbanken login.");
        poll_delay(init.initial_sleep_time).await;
        for _ in 0..180 {
            let authenticate = at("poll", link(&links, "authenticate"))?;
            let response: Poll = at(
                "poll",
                parse(at(
                    "poll",
                    browser_post(page, &authenticate, HEADERS, &serde_json::json!({})).await,
                )?),
            )?;
            let action = poll_action(&response.result)?;
            if matches!(action, PollAction::Authenticated) {
                return at("handoff", next_step(&response.links));
            }
            links.extend(response.links);
            match action {
                PollAction::Continue => {
                    if response.result == "IN_PROGRESS" {
                        presenter.present_status("Confirm the login in BankID…");
                    }
                    if let Some(qr) = &response.qr_start_token {
                        presenter.present_qr(qr);
                    }
                }
                PollAction::Refresh => {
                    let challenge_url = at("refresh", link(&links, "getChallenge"))?;
                    let challenge: Challenge = at(
                        "refresh",
                        parse(at(
                            "refresh",
                            browser_get(page, &challenge_url, HEADERS).await,
                        )?),
                    )?;
                    presenter.present_qr(&challenge.qr_start_token);
                    poll_delay(challenge.initial_sleep_time).await;
                    continue;
                }
                PollAction::Authenticated => unreachable!(),
            }
            poll_delay(response.iteration_sleep_time).await;
        }
        Err(LoginFailure {
            stage: "poll",
            reason: "timed_out",
        })
    }
    .await;

    if auth_result.is_err()
        && let Some(cancel) = &cancel
    {
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            browser_post(page, cancel, HEADERS, &serde_json::Value::Null),
        )
        .await;
    }
    auth_result
}

/// Open the bank's company application and retain only the headers needed for
/// later account reads. Their values are never serialized or logged.
pub(crate) async fn open_application(
    page: &chromiumoxide::page::Page,
    personnummer: &str,
    presenter: &dyn AuthPresenter,
) -> Result<Vec<(&'static str, String)>> {
    if personnummer.len() != 12 || !personnummer.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(PrickaError::AuthFailed(
            "A 12-digit personnummer is required".into(),
        ));
    }
    let mut requests = page
        .event_listener::<EventRequestWillBeSent>()
        .await
        .map_err(|_| PrickaError::Browser("Company request listener failed".into()))?;
    let handoff = tokio::time::timeout(
        Duration::from_secs(180),
        authenticate_handoff(page, personnummer, presenter),
    )
    .await
    .map_err(|_| PrickaError::AuthTimeout)?
    .map_err(|failure| {
        PrickaError::AuthFailed(format!(
            "Company login failed at {}: {}",
            failure.stage, failure.reason
        ))
    })?;
    if !matches!(handoff.next, NextStep::ApplicationEntry) {
        return Err(PrickaError::NotImplemented(
            "Company selection requires a separate bank flow".into(),
        ));
    }
    page.goto(&handoff.url)
        .await
        .map_err(|_| PrickaError::Browser("Company application did not open".into()))?;
    tokio::time::sleep(Duration::from_secs(4)).await;
    for _ in 0..300 {
        let Ok(Some(request)) =
            tokio::time::timeout(Duration::from_millis(10), requests.next()).await
        else {
            break;
        };
        if let Ok(url) = url::Url::parse(&request.request.url)
            && url.scheme() == "https"
            && url.host_str() == Some("secure.handelsbanken.se")
            && url.path().contains("/rykk/bu/accounts/")
            && request.request.method == "GET"
        {
            return Ok(bank_request_headers(request.request.headers.inner()));
        }
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bank_links_stay_on_bank_origin() {
        assert!(bank_url("/logon/se/corp/sv/").is_ok());
        assert!(bank_url("https://example.com/redirect").is_err());
        assert!(bank_url("https://user:password@secure.handelsbanken.se/").is_err());
    }

    #[test]
    fn company_selection_is_explicitly_distinguished() {
        let mut links = HashMap::new();
        links.insert(
            "getMandates".into(),
            Link {
                href: "/select".into(),
            },
        );
        assert!(matches!(
            next_step(&links).unwrap().next,
            NextStep::CompanySelection
        ));
    }

    #[test]
    fn request_headers_include_only_known_token_names() {
        let headers = serde_json::json!({
            "X-Shb-Synchronized-Token": "synthetic",
            "cookie": "private",
        });
        assert_eq!(
            bank_request_headers(&headers),
            vec![("x-shb-synchronized-token", "synthetic".to_owned())]
        );
    }
}
