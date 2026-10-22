//! The only network surface. Requests go to a fixed allowlist of HTTPS hosts and
//! never follow redirects (a bearer token must not reach another host). Errors
//! carry at most a short server message, never a response body, which can
//! contain personal data.

use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::ACCEPT;
use reqwest::Url;
use serde_json::Value;
use std::time::Duration;

/// `cloud.ouraring.com` is only probed (no credentials) to check an app's
/// authorization settings before login.
pub const ALLOWED_HOSTS: &[&str] =
    &["api.ouraring.com", "moi.ouraring.com", "cloud.ouraring.com", "auth-api.8slp.net", "client-api.8slp.net"];

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{service} HTTP {status}{}", suffix(.detail))]
    Status { service: &'static str, status: u16, detail: Option<String> },
    #[error("{service} network request failed: {reason}")]
    Network { service: &'static str, reason: String },
    #[error("{service} returned a response that is not a JSON object")]
    InvalidJson { service: &'static str },
    #[error("refusing request to unexpected destination {0}")]
    Destination(String),
}

fn suffix(detail: &Option<String>) -> String {
    detail.as_deref().map(|d| format!(" ({d})")).unwrap_or_default()
}

impl ApiError {
    pub fn status(&self) -> Option<u16> {
        match self {
            ApiError::Status { status, .. } => Some(*status),
            _ => None,
        }
    }
}

pub fn client() -> anyhow::Result<Client> {
    Ok(Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("somno/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

pub fn check_destination(url: &Url) -> Result<(), ApiError> {
    let ok = url.scheme() == "https"
        && url.host_str().is_some_and(|h| ALLOWED_HOSTS.contains(&h))
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none();
    if ok {
        Ok(())
    } else {
        Err(ApiError::Destination(url.origin().ascii_serialization()))
    }
}

pub fn get(client: &Client, service: &'static str, url: &Url, bearer: &str) -> Result<Value, ApiError> {
    check_destination(url)?;
    send(service, "GET", url, client.get(url.clone()).bearer_auth(bearer), &[bearer])
}

pub fn post_form(client: &Client, service: &'static str, url: &Url, form: &[(&str, &str)]) -> Result<Value, ApiError> {
    check_destination(url)?;
    send(service, "POST", url, client.post(url.clone()).form(form), &form_secrets(form))
}

pub fn post_json(client: &Client, service: &'static str, url: &Url, body: &Value) -> Result<Value, ApiError> {
    check_destination(url)?;
    send(service, "POST", url, client.post(url.clone()).json(body), &json_secrets(body))
}

/// Status code of a GET, without following redirects or parsing the body.
pub fn get_status(client: &Client, service: &'static str, url: &Url) -> Result<u16, ApiError> {
    check_destination(url)?;
    Ok(exchange(service, "GET", url, client.get(url.clone()), &[])?.0)
}

/// POST a form and return the status with any JSON body, error statuses
/// included. For probes that interpret the error themselves.
pub fn post_form_outcome(client: &Client, service: &'static str, url: &Url, form: &[(&str, &str)]) -> Result<(u16, Option<Value>), ApiError> {
    check_destination(url)?;
    let (status, body) = exchange(service, "POST", url, client.post(url.clone()).form(form), &form_secrets(form))?;
    Ok((status, serde_json::from_slice::<Value>(&body).ok().filter(Value::is_object)))
}

/// Values of a form's secret fields (tokens, codes, client secret, password).
fn form_secrets<'a>(form: &[(&str, &'a str)]) -> Vec<&'a str> {
    form.iter().filter(|(k, _)| crate::log::is_secret_name(k)).map(|(_, v)| *v).collect()
}

fn json_secrets(body: &Value) -> Vec<&str> {
    body.as_object()
        .map(|o| o.iter().filter(|(k, _)| crate::log::is_secret_name(k)).filter_map(|(_, v)| v.as_str()).collect())
        .unwrap_or_default()
}

fn send(service: &'static str, method: &str, url: &Url, request: RequestBuilder, secrets: &[&str]) -> Result<Value, ApiError> {
    let (status, body) = exchange(service, method, url, request, secrets)?;
    if !(200..300).contains(&status) {
        return Err(ApiError::Status { service, status, detail: error_message(&body, secrets) });
    }
    match serde_json::from_slice::<Value>(&body) {
        Ok(v) if v.is_object() => Ok(v),
        _ => Err(ApiError::InvalidJson { service }),
    }
}

/// Every request goes through here: send, read the body, log one line.
fn exchange(service: &'static str, method: &str, url: &Url, request: RequestBuilder, secrets: &[&str]) -> Result<(u16, Vec<u8>), ApiError> {
    let started = std::time::Instant::now();
    let target = crate::log::redact_url(url);
    let network = |e: reqwest::Error| {
        let reason = e.without_url().to_string();
        crate::log::event("http", format_args!("{method} {target} -> network error: {reason}"));
        ApiError::Network { service, reason }
    };
    let response = request.header(ACCEPT, "application/json").send().map_err(network)?;
    let status = response.status().as_u16();
    let body = response.bytes().map_err(network)?.to_vec();
    let ms = started.elapsed().as_millis();
    let error = if (200..300).contains(&status) { String::new() } else { error_summary(&body, secrets) };
    crate::log::event("http", format_args!("{method} {target} -> {status} ({ms} ms, {} bytes){error}", body.len()));
    Ok((status, body))
}

/// ` error=<code> message=<short text>` from a JSON error body, for the log,
/// with any secret the request sent removed.
pub fn error_summary(body: &[u8], secrets: &[&str]) -> String {
    let code = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(Value::as_str).map(|c| crate::log::scrub(c, secrets).chars().filter(|c| !c.is_control()).take(60).collect::<String>()));
    let mut out = String::new();
    if let Some(code) = code {
        out += &format!(" error={code}");
    }
    if let Some(message) = error_message(body, secrets) {
        out += &format!(" message={message:?}");
    }
    out
}

/// `short_message` with any secret the request sent, and any `key=value`
/// secret, removed before it is shortened.
pub fn error_message(body: &[u8], secrets: &[&str]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    ["error_description", "message", "detail", "error"].iter().find_map(|k| {
        let text = crate::log::scrub(v.get(*k)?.as_str()?, secrets);
        let clean: String = text.chars().filter(|c| !c.is_control()).take(160).collect();
        (!clean.trim().is_empty()).then_some(clean)
    })
}

/// A short human-readable error string from a JSON error body, if there is one.
pub fn short_message(body: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    // The first of these keys holding a non-blank string; a blank one doesn't hide the rest.
    ["error_description", "message", "detail", "error"].iter().find_map(|k| {
        let clean: String = v.get(*k)?.as_str()?.chars().filter(|c| !c.is_control()).take(160).collect();
        (!clean.trim().is_empty()).then_some(clean)
    })
}
