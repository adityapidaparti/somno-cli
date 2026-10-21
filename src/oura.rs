//! Oura API V2, read-only, via your own developer application (OAuth
//! authorization-code flow; personal access tokens were retired in Dec 2025).
//!
//! Field names follow Oura OpenAPI 1.42 (checked 2026-10-20). Refresh tokens
//! are single-use, so every refresh happens under a file lock and the rotated
//! pair is saved before the new access token is used.
//!
//! Token endpoint: Oura's docs name `api.ouraring.com/oauth/token`, but apps
//! from the new developer portal are rejected there (refreshes failing with
//! 400 since 2026-10-09, reported independently by the Home Assistant Oura
//! integration PR #76, HealthLog #1204 and oura-reader PR #4). The live
//! endpoint is `moi.ouraring.com`, so it is tried first: an authorization code
//! is single-use, and a rejection by the wrong endpoint would burn it. The
//! documented endpoint is the fallback for legacy-portal apps.

use anyhow::{anyhow, bail, Context, Result};
use chrono::{Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use reqwest::blocking::Client;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Instant;

use crate::http::{self, ApiError};
use crate::night::{day_of, num, text, Night};
use crate::log;
use crate::store;
use crate::types::parse_timestamp;

pub const AUTHORIZE_URL: &str = "https://cloud.ouraring.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://moi.ouraring.com/oauth/v2/ext/oauth-token";
pub const TOKEN_URL_LEGACY: &str = "https://api.ouraring.com/oauth/token";
pub const API_BASE: &str = "https://api.ouraring.com/v2/usercollection/";
/// The developer portal accepts plain HTTP only for the literal `localhost`
/// host; the listener itself binds loopback only.
pub const REDIRECT_URI: &str = "http://localhost:8765/callback";
const CALLBACK_BIND: &str = "127.0.0.1:8765";
const CLIENT_FILE: &str = "oura-client.json";
const TOKENS_FILE: &str = "oura-tokens.json";
const LOCK_FILE: &str = "oura.lock";
const SERVICE: &str = "Oura";

pub const COLLECTIONS: &[&str] = &[
    "daily_activity", "daily_cardiovascular_age", "daily_readiness", "daily_resilience",
    "daily_sleep", "daily_spo2", "daily_stress", "enhanced_tag", "heartrate", "personal_info",
    "rest_mode_period", "ring_battery_level", "ring_configuration", "session", "sleep",
    "sleep_time", "tag", "vO2_max", "workout",
];
/// Queried with `start_datetime`/`end_datetime` instead of dates.
pub const TIMESERIES: &[&str] = &["heartrate", "ring_battery_level"];
/// Not date-filtered.
pub const UNDATED: &[&str] = &["personal_info", "ring_configuration"];

/// Every scope the developer portal offers (2026-10-20). Oura's docs say a
/// blank `scope` requests "all available scopes" but list only eight, without
/// `stress`, `heart_health` or `ring_configuration`, so all are requested
/// explicitly, as the Home Assistant integration and oura-reader do.
pub const SCOPES: &[&str] = &[
    "email", "personal", "daily", "heartrate", "workout", "tag", "session", "spo2",
    "ring_configuration", "stress", "heart_health",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: i64,
    #[serde(default)]
    pub scope: Option<String>,
}

pub fn load_client() -> Result<Option<ClientConfig>> {
    store::read(CLIENT_FILE)
}

pub fn save_client(cfg: &ClientConfig) -> Result<std::path::PathBuf> {
    store::write(CLIENT_FILE, cfg)
}

pub fn load_tokens() -> Result<Option<Tokens>> {
    store::read(TOKENS_FILE)
}

pub fn logout() -> Result<bool> {
    let _lock = store::lock(LOCK_FILE)?;
    store::remove(TOKENS_FILE)
}

pub fn authorize_url(client_id: &str, state: &str) -> String {
    authorize_url_with(client_id, state, REDIRECT_URI)
}

fn authorize_url_with(client_id: &str, state: &str, redirect_uri: &str) -> String {
    let scope = SCOPES.join(" ");
    Url::parse_with_params(
        AUTHORIZE_URL,
        &[
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", redirect_uri),
            ("scope", &scope),
            ("state", state),
        ],
    )
    .expect("static URL")
    .to_string()
}

/// Requested scopes missing from a granted `scope` string (space-separated).
/// Oura reports them prefixed (`extapi:daily`), so the prefix is ignored.
/// Unknown when Oura didn't report what it granted.
pub fn missing_scopes(granted: Option<&str>) -> Option<Vec<&'static str>> {
    let granted: HashSet<&str> = granted?
        .split_whitespace()
        .map(|s| s.rsplit_once(':').map_or(s, |(_, name)| name))
        .collect();
    Some(SCOPES.iter().copied().filter(|s| !granted.contains(s)).collect())
}

// ---- configuration checks (no login needed) ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Pass(String),
    Fail(String),
    /// The probe could not decide (network error, unexpected answer).
    Unknown(String),
}

/// The authorization page validates the client and redirect URI before
/// showing a sign-in screen. A rejection here means the browser login would
/// fail the same way. An unknown client ID still passes this check (Oura
/// sends it to sign-in), which is why the secret check also runs.
pub fn interpret_authorize(status: u16) -> Check {
    match status {
        200 | 301..=303 | 307 | 308 => Check::Pass("authorization request accepted".into()),
        400 => Check::Fail(format!(
            "Oura rejected the authorization request for this app (400 invalid_request), so a browser login would fail too; most likely the app's Redirect URI is not exactly {REDIRECT_URI}"
        )),
        s => Check::Unknown(format!("authorization page answered HTTP {s}")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientProbe {
    /// The endpoint authenticated the client; only the fake code was refused.
    Accepted,
    /// `invalid_client`: wrong client ID or secret for this endpoint.
    Rejected,
    Unclear(String),
}

/// Interpret a token request made with a deliberately invalid code. Clients
/// are authenticated before the code is looked at (RFC 6749 §5.2), so
/// `invalid_grant` means the ID and secret are right.
pub fn interpret_token_probe(status: u16, body: Option<&Value>) -> ClientProbe {
    let error = body.and_then(|b| text(b, &["error"]));
    match (status, error) {
        (_, Some("invalid_client")) | (401, _) => ClientProbe::Rejected,
        (400, Some("invalid_grant")) => ClientProbe::Accepted,
        (s, e) => ClientProbe::Unclear(format!("HTTP {s}{}", e.map(|e| format!(" {e}")).unwrap_or_default())),
    }
}

/// Redirect URIs one typo away from the right one. When the exact URI is
/// rejected, these are tried to tell the user what the app actually has.
pub const REDIRECT_NEAR_MISSES: &[&str] = &[
    "http://localhost:8765/callback.",
    "http://localhost:8765/callback/",
    "http://127.0.0.1:8765/callback",
    "https://localhost:8765/callback",
    "http://localhost:8765",
    "http://localhost:8765/",
    "http://localhost:8765/callback,",
];

/// Check a client configuration against Oura without logging in: the
/// authorization page (client ID + redirect URI) and the token endpoints
/// (client ID + secret, using a code that cannot be valid).
pub fn verify_client(http: &Client, cfg: &ClientConfig) -> (Check, Check) {
    let status_for = |redirect: &str| {
        let url = Url::parse(&authorize_url_with(&cfg.client_id, "somno-configure-check", redirect)).expect("built from static URL");
        http::get_status(http, SERVICE, &url)
    };
    let authorize = match status_for(REDIRECT_URI) {
        Ok(400) => {
            let registered = REDIRECT_NEAR_MISSES
                .iter()
                .find(|candidate| matches!(status_for(candidate).map(interpret_authorize), Ok(Check::Pass(_))));
            match registered {
                Some(found) => Check::Fail(format!(
                    "the app's Redirect URI is registered as {found:?}; edit it in the developer portal to exactly {REDIRECT_URI:?}"
                )),
                None => interpret_authorize(400),
            }
        }
        Ok(status) => interpret_authorize(status),
        Err(e) => Check::Unknown(e.to_string()),
    };
    let form = [
        ("grant_type", "authorization_code"),
        ("code", "somno-configure-check"),
        ("redirect_uri", REDIRECT_URI),
        ("client_id", cfg.client_id.as_str()),
        ("client_secret", cfg.client_secret.as_str()),
    ];
    let probe = |url: &str| match http::post_form_outcome(http, SERVICE, &Url::parse(url).expect("static URL"), &form) {
        Ok((status, body)) => interpret_token_probe(status, body.as_ref()),
        Err(e) => ClientProbe::Unclear(e.to_string()),
    };
    let secret = match probe(TOKEN_URL) {
        ClientProbe::Accepted => Check::Pass("client ID and secret accepted".into()),
        ClientProbe::Unclear(why) => Check::Unknown(format!("token endpoint gave an unexpected answer ({why})")),
        // Some legacy-portal apps are only known to the documented endpoint.
        ClientProbe::Rejected => match probe(TOKEN_URL_LEGACY) {
            ClientProbe::Accepted => Check::Pass("client ID and secret accepted (legacy endpoint)".into()),
            ClientProbe::Rejected => Check::Fail("Oura rejected the client ID or secret (invalid_client): copy both again from the developer portal".into()),
            ClientProbe::Unclear(why) => Check::Fail(format!(
                "the live token endpoint rejected the client ID or secret (invalid_client); the legacy one answered {why}"
            )),
        },
    };
    (authorize, secret)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Callback {
    Code(String),
    /// The user declined on Oura's consent screen (state matched).
    Denied(String),
    /// Not our callback: wrong path, wrong/missing state, repeated parameters.
    Invalid(&'static str),
}

/// Validate the request target of the OAuth redirect, e.g. `/callback?code=…&state=…`.
pub fn parse_callback(target: &str, expected_state: &str) -> Callback {
    let Ok(url) = Url::parse(&format!("http://localhost{target}")) else {
        return Callback::Invalid("unparseable request");
    };
    if url.path() != "/callback" {
        return Callback::Invalid("unexpected path");
    }
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let mut seen = HashSet::new();
    if !pairs.iter().all(|(k, _)| seen.insert(k.as_str())) {
        return Callback::Invalid("repeated parameter");
    }
    let get = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    match get("state") {
        Some(state) if !expected_state.is_empty() && constant_time_eq(state.as_bytes(), expected_state.as_bytes()) => {}
        _ => return Callback::Invalid("state mismatch"),
    }
    if let Some(error) = get("error") {
        return Callback::Denied(error.chars().filter(|c| !c.is_control()).take(80).collect());
    }
    match get("code") {
        Some(code) if !code.is_empty() => Callback::Code(code.to_string()),
        _ => Callback::Invalid("missing code"),
    }
}

fn callback_kind(c: &Callback) -> String {
    match c {
        Callback::Code(_) => "code".into(),
        Callback::Denied(reason) => format!("denied: {reason}"),
        Callback::Invalid(why) => format!("invalid: {why}"),
    }
}

/// Accept one pending callback connection, if any, and answer it.
fn accept_callback(listener: &TcpListener, state: &str) -> Result<Option<Callback>> {
    let mut stream = match listener.accept() {
        Ok((stream, _)) => stream,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf).unwrap_or(0);
    let request = String::from_utf8_lossy(&buf[..n]);
    let target = request.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("");
    let outcome = parse_callback(target, state);
    let path = target.split('?').next().unwrap_or("");
    log::event("oura.login", format_args!("callback on {path:?}: {}", callback_kind(&outcome)));
    let (status, body) = match &outcome {
        Callback::Code(_) => ("200 OK", "Authorized. You can close this tab and return to the terminal."),
        Callback::Denied(_) => ("200 OK", "Authorization was declined. Return to the terminal."),
        Callback::Invalid(_) => ("400 Bad Request", "Not a valid callback for this login attempt."),
    };
    let _ = write!(stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len());
    Ok(Some(outcome))
}

/// Lines typed or pasted into the terminal, read on a background thread so the
/// listener keeps polling. The thread ends at EOF (no terminal attached).
fn stdin_lines() -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// Open the consent page locally only on a Mac with a local session.
fn should_open_browser() -> bool {
    let remote = ["SSH_CONNECTION", "SSH_TTY", "SSH_CLIENT"].iter().any(|v| std::env::var_os(v).is_some());
    cfg!(target_os = "macos") && !remote && std::env::var_os("SOMNO_NO_BROWSER").is_none()
}

/// A redirect URL pasted from a browser's address bar, validated like the
/// loopback callback (same state check). Accepts the full URL, the URL without
/// its scheme, the path, or just the query string.
pub fn parse_pasted(input: &str, expected_state: &str) -> Callback {
    let s = input.trim().trim_matches(|c| matches!(c, '"' | '\'' | '<' | '>'));
    let with_scheme = if s.starts_with("localhost") { format!("http://{s}") } else { s.to_string() };
    let target = if with_scheme.starts_with("http://") || with_scheme.starts_with("https://") {
        let Ok(url) = Url::parse(&with_scheme) else {
            return Callback::Invalid("unparseable URL");
        };
        if url.host_str() != Some("localhost") || url.port_or_known_default() != Some(8765) {
            return Callback::Invalid("not the registered redirect address");
        }
        format!("{}?{}", url.path(), url.query().unwrap_or(""))
    } else if s.starts_with('/') {
        s.to_string()
    } else if s.starts_with('?') || s.contains("code=") || s.contains("error=") {
        format!("/callback?{}", s.trim_start_matches('?'))
    } else {
        return Callback::Invalid("expected the full redirect URL");
    };
    parse_callback(&target, expected_state)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn random_state() -> Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Validate a token-endpoint response. A missing refresh token keeps the
/// previous one; a missing scope keeps the previous scope.
pub fn tokens_from_response(v: &Value, now: i64, previous: Option<&Tokens>) -> Result<Tokens> {
    let access_token = text(v, &["access_token"]).context("Oura token response has no access token")?;
    let refresh_token = text(v, &["refresh_token"])
        .map(str::to_string)
        .or_else(|| previous.map(|p| p.refresh_token.clone()))
        .context("Oura token response has no refresh token")?;
    let expires_in = num(v, &["expires_in"]).filter(|s| *s > 0.0).context("Oura token response has no valid expiry")?;
    if let Some(kind) = text(v, &["token_type"]) {
        if !kind.eq_ignore_ascii_case("bearer") {
            bail!("unexpected Oura token type {kind}");
        }
    }
    Ok(Tokens {
        access_token: access_token.to_string(),
        refresh_token,
        // Saturating, so an absurd expiry can't overflow; capped at ten years.
        expires_at: now.saturating_add(expires_in.min(10.0 * 365.0 * 86400.0) as i64),
        scope: text(v, &["scope"]).map(str::to_string).or_else(|| previous.and_then(|p| p.scope.clone())),
    })
}

/// Collect `data` from every page, following `next_token` until it is null.
pub fn paginate(mut fetch: impl FnMut(Option<&str>) -> Result<Value>) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut token: Option<String> = None;
    for _ in 0..10_000 {
        let page = fetch(token.as_deref())?;
        let data = page.get("data").and_then(Value::as_array).context("Oura response is missing its data array")?;
        out.extend(data.iter().cloned());
        match page.get("next_token") {
            None | Some(Value::Null) => return Ok(out),
            Some(Value::String(t)) if !t.is_empty() => {
                if !seen.insert(t.clone()) {
                    bail!("Oura repeated a pagination token");
                }
                token = Some(t.clone());
            }
            Some(_) => bail!("Oura returned an invalid pagination token"),
        }
    }
    bail!("Oura pagination exceeded 10000 pages")
}

/// Query parameters for a collection. Daily collections get Oura's own
/// `start_date`/`end_date`; time series get midnight-to-midnight datetimes
/// in `tz` (with the correct offset across DST changes).
pub fn window_params(collection: &str, start: Option<NaiveDate>, end: Option<NaiveDate>, tz: Tz) -> Result<Vec<(String, String)>> {
    if UNDATED.contains(&collection) {
        return Ok(vec![]);
    }
    let midnight = |d: NaiveDate| -> Result<String> {
        let local = tz
            .from_local_datetime(&d.and_time(NaiveTime::MIN))
            .earliest()
            .with_context(|| format!("{d} has no midnight in {}", tz.name()))?;
        Ok(local.to_rfc3339())
    };
    let mut params = Vec::new();
    let timeseries = TIMESERIES.contains(&collection);
    if let Some(s) = start {
        params.push(if timeseries { ("start_datetime".into(), midnight(s)?) } else { ("start_date".into(), s.to_string()) });
    }
    if let Some(e) = end {
        params.push(if timeseries { ("end_datetime".into(), midnight(e)?) } else { ("end_date".into(), e.to_string()) });
    }
    Ok(params)
}

fn with_hint(e: ApiError) -> anyhow::Error {
    let hint = match e.status() {
        Some(401) => ": run `somno oura login`",
        Some(403) => ": check the scopes you granted and that your Oura membership is active",
        Some(429) => ": rate limited, retry later",
        _ => "",
    };
    anyhow!("{e}{hint}")
}

pub struct Oura {
    http: Client,
    cfg: ClientConfig,
}

impl Oura {
    pub fn load(http: Client) -> Result<Self> {
        let cfg = load_client()?.context("Oura is not configured: run `somno oura configure`")?;
        Ok(Self { http, cfg })
    }

    /// POST to the live token endpoint, then to the legacy one if the live one
    /// rejects the client (400/401). When both fail, the live endpoint's error
    /// is reported.
    fn token_request(&self, form: &[(&str, &str)]) -> Result<Value, ApiError> {
        let mut full = form.to_vec();
        full.push(("client_id", &self.cfg.client_id));
        full.push(("client_secret", &self.cfg.client_secret));
        let post = |url: &str| http::post_form(&self.http, SERVICE, &Url::parse(url).expect("static URL"), &full);
        match post(TOKEN_URL) {
            Err(e) if matches!(e.status(), Some(400 | 401)) => {
                log::event("oura.token", format_args!("live endpoint refused ({e}); trying legacy endpoint"));
                post(TOKEN_URL_LEGACY).map_err(|_| e)
            }
            other => other,
        }
    }

    /// Browser consent, then exchange the code. The redirect is caught on the
    /// loopback listener when the browser runs on this machine; otherwise the
    /// user pastes the redirect URL from the address bar (SSH, a server, a bot's
    /// host). Whichever arrives first wins. Waits up to ten minutes.
    pub fn login(&self) -> Result<Tokens> {
        let _lock = store::lock(LOCK_FILE)?;
        let state = random_state()?;
        let listener = match TcpListener::bind(CALLBACK_BIND) {
            Ok(l) => {
                l.set_nonblocking(true)?;
                log::event("oura.login", format_args!("listening on {CALLBACK_BIND}"));
                Some(l)
            }
            Err(e) => {
                log::event("oura.login", format_args!("cannot listen on {CALLBACK_BIND}: {e}; paste only"));
                eprintln!("note: not listening on {CALLBACK_BIND} ({e}); paste the redirect URL instead");
                None
            }
        };
        let url = authorize_url(&self.cfg.client_id, &state);
        eprintln!("1. Open this URL in any browser and approve access:\n\n{url}\n");
        eprintln!("2. If that browser is on this machine, you're done; this finishes by itself.");
        eprintln!("   Otherwise the browser ends on a page that fails to load. Copy its full address");
        eprintln!("   ({REDIRECT_URI}?code=...) and paste it here, then press Enter.\n");
        if should_open_browser() {
            let _ = std::process::Command::new("open")
                .arg(&url)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        let pasted = stdin_lines();
        let mut stdin_open = true;
        let deadline = Instant::now() + std::time::Duration::from_secs(600);
        let code = loop {
            let mut outcome = match &listener {
                Some(l) => accept_callback(l, &state)?,
                None => None,
            };
            if outcome.is_none() && stdin_open {
                match pasted.try_recv() {
                    Ok(line) if line.trim().is_empty() => {}
                    Ok(line) => match parse_pasted(&line, &state) {
                        Callback::Invalid(why) => {
                            log::event("oura.login", format_args!("pasted text rejected: {why}"));
                            eprintln!("not a redirect URL for this login ({why}); paste the full address again")
                        }
                        valid => {
                            log::event("oura.login", format_args!("pasted redirect accepted ({})", callback_kind(&valid)));
                            outcome = Some(valid)
                        }
                    },
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => stdin_open = false,
                }
            }
            match outcome {
                Some(Callback::Code(code)) => break code,
                Some(Callback::Denied(reason)) => bail!("Oura authorization was declined ({reason})"),
                _ => {}
            }
            if listener.is_none() && !stdin_open {
                bail!("cannot receive the Oura redirect: port {CALLBACK_BIND} is unavailable and there is no terminal to paste into");
            }
            if Instant::now() > deadline {
                bail!("timed out waiting for the Oura redirect; run `somno oura login` again");
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        let v = self
            .token_request(&[("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", REDIRECT_URI)])
            .map_err(|e| anyhow!("{e}: the code exchange was rejected; check the client secret and the registered redirect URI"))?;
        let tokens = tokens_from_response(&v, Utc::now().timestamp(), None)?;
        store::write(TOKENS_FILE, &tokens)?;
        log::event("oura.login", format_args!(
            "tokens saved; expires_at={} scopes={:?} missing={:?}",
            tokens.expires_at, tokens.scope, missing_scopes(tokens.scope.as_deref())
        ));
        Ok(tokens)
    }

    /// A valid access token, refreshing first when it expires within a minute.
    /// `rejected` is a token the API just refused: refresh unless another
    /// process has already replaced it.
    fn access_token(&self, rejected: Option<&str>) -> Result<String> {
        let _lock = store::lock(LOCK_FILE)?;
        let tokens = load_tokens()?.context("not logged in to Oura: run `somno oura login`")?;
        let now = Utc::now().timestamp();
        let usable = match rejected {
            Some(bad) => tokens.access_token != bad,
            None => tokens.expires_at > now + 60,
        };
        if usable {
            return Ok(tokens.access_token);
        }
        log::event("oura.refresh", if rejected.is_some() { "access token rejected by API; refreshing" } else { "access token expiring; refreshing" });
        let v = self
            .token_request(&[("grant_type", "refresh_token"), ("refresh_token", &tokens.refresh_token)])
            .map_err(|e| match e.status() {
                Some(400 | 401) => anyhow!("{e}: Oura rejected the saved refresh token; run `somno oura login`"),
                _ => with_hint(e),
            })?;
        let fresh = tokens_from_response(&v, now, Some(&tokens))?;
        store::write(TOKENS_FILE, &fresh)?;
        log::event("oura.refresh", format_args!("rotated; expires_at={}", fresh.expires_at));
        Ok(fresh.access_token)
    }

    fn get(&self, url: &Url) -> Result<Value> {
        let token = self.access_token(None)?;
        match http::get(&self.http, SERVICE, url, &token) {
            Err(e) if e.status() == Some(401) => {
                let fresh = self.access_token(Some(&token))?;
                http::get(&self.http, SERVICE, url, &fresh).map_err(with_hint)
            }
            other => other.map_err(with_hint),
        }
    }

    fn url(collection: &str, params: &[(String, String)], next: Option<&str>) -> Result<Url> {
        if !COLLECTIONS.contains(&collection) {
            bail!("unknown Oura collection '{collection}' (one of: {})", COLLECTIONS.join(", "));
        }
        let mut url = Url::parse(API_BASE).expect("static URL").join(collection)?;
        if !params.is_empty() || next.is_some() {
            let mut query = url.query_pairs_mut();
            for (k, v) in params {
                query.append_pair(k, v);
            }
            if let Some(t) = next {
                query.append_pair("next_token", t);
            }
        }
        Ok(url)
    }

    /// Every document in a collection (all pages). `personal_info` is a single
    /// document and is returned as a one-element list.
    pub fn collection(&self, collection: &str, params: &[(String, String)]) -> Result<Vec<Value>> {
        if collection == "personal_info" {
            return Ok(vec![self.get(&Self::url(collection, params, None)?)?]);
        }
        paginate(|next| self.get(&Self::url(collection, params, next)?))
    }

    /// Read the first page of every collection over the last few days, to show
    /// which data the granted scopes actually reach. Returns (collection,
    /// documents on the first page or the error).
    pub fn access_check(&self, today: NaiveDate, tz: Tz) -> Vec<(&'static str, Result<usize, String>)> {
        COLLECTIONS
            .iter()
            .map(|&name| {
                let result = window_params(name, Some(today - Duration::days(3)), Some(today + Duration::days(1)), tz)
                    .and_then(|params| self.get(&Self::url(name, &params, None)?))
                    .map(|page| page.get("data").and_then(Value::as_array).map(Vec::len).unwrap_or(1))
                    .map_err(|e| format!("{e:#}"));
                (name, result)
            })
            .collect()
    }

    /// Documents of a collection that belong to `start..=end`, oldest first.
    /// The query is padded a day each side and filtered by each document's
    /// own day, so Oura's end-date semantics don't matter.
    pub fn docs_in(&self, collection: &str, start: NaiveDate, end: NaiveDate) -> Result<Vec<Value>> {
        let (from, to) = query_window(collection, start, end);
        let params = vec![("start_date".to_string(), from.to_string()), ("end_date".to_string(), to.to_string())];
        Ok(crate::views::select(collection, self.collection(collection, &params)?, start, end))
    }

    /// Sleep, daily sleep score and readiness for `days`, summarized per night.
    /// The query window is padded by a day on each side and filtered by Oura's
    /// own `day` label, so no assumption about end-date inclusivity is needed.
    /// Returns notes for collections that could not be read.
    pub fn nights(&self, days: &[NaiveDate]) -> Result<(BTreeMap<NaiveDate, Night>, Vec<String>)> {
        let (Some(first), Some(last)) = (days.first(), days.last()) else {
            return Ok((BTreeMap::new(), vec![]));
        };
        let params = vec![
            ("start_date".to_string(), (*first - Duration::days(1)).to_string()),
            ("end_date".to_string(), (*last + Duration::days(1)).to_string()),
        ];
        let sleep = self.collection("sleep", &params)?;
        let mut notes = Vec::new();
        let mut optional = |name: &str| match self.collection(name, &params) {
            Ok(docs) => docs,
            Err(e) => {
                notes.push(format!("Oura {name} unavailable: {e}"));
                vec![]
            }
        };
        let daily_sleep = optional("daily_sleep");
        let readiness = optional("daily_readiness");
        Ok((summarize(&sleep, &daily_sleep, &readiness, days), notes))
    }
}

/// The longest period. Ties go to the earliest bedtime, then the smallest
/// id, then the document text, so the pick never depends on the order Oura
/// returns documents in.
pub fn longest<'a>(periods: impl Iterator<Item = &'a Value>) -> Option<&'a Value> {
    periods.max_by(|a, b| {
        let total = |d: &Value| num(d, &["total_sleep_duration"]).unwrap_or(-1.0);
        let start = |d: &Value| text(d, &["bedtime_start"]).and_then(parse_timestamp).map(|t| t.with_timezone(&Utc));
        let id = |d: &Value| text(d, &["id"]).unwrap_or("").to_string();
        total(a)
            .total_cmp(&total(b))
            .then_with(|| start(b).cmp(&start(a)))
            .then_with(|| id(b).cmp(&id(a)))
            .then_with(|| b.to_string().cmp(&a.to_string()))
    })
}

/// Whether a period is sleep the user has. Oura OpenAPI 1.42: `deleted` is
/// deleted by the user and `rest` is "falsely detected sleep / nap, rejected
/// in confirm prompt by user"; both stay visible in `somno sleep` but never
/// count as sleep.
pub fn is_confirmed_sleep(doc: &Value) -> bool {
    !matches!(text(doc, &["type"]), Some("deleted" | "rest"))
}

/// The main period of a day: the longest `long_sleep`, else the longest
/// other confirmed period.
pub fn main_period<'a>(periods: &[&'a Value]) -> Option<&'a Value> {
    let live = || periods.iter().copied().filter(|d| is_confirmed_sleep(d));
    longest(live().filter(|d| text(d, &["type"]) == Some("long_sleep"))).or_else(|| longest(live()))
}

/// Collections whose documents are spans (`start_day`..`end_day`). A span
/// can start long before the range it overlaps.
pub const SPANS: &[&str] = &["rest_mode_period", "enhanced_tag"];

/// How far back to look for spans that started before the range. Longer
/// spans that started earlier are missed.
pub const SPAN_LOOKBACK_DAYS: i64 = 365;

/// The `start_date`/`end_date` to ask Oura for when the caller wants the
/// documents of `start..=end`: one extra day each side, so the result does not
/// depend on whether Oura's bounds are inclusive, and a year back for span
/// collections, since Oura presumably filters spans by their start day.
pub fn query_window(collection: &str, start: NaiveDate, end: NaiveDate) -> (NaiveDate, NaiveDate) {
    let back = if SPANS.contains(&collection) { SPAN_LOOKBACK_DAYS } else { 1 };
    (start - Duration::days(back), end + Duration::days(1))
}

/// The first document for each day (Oura sends one daily document per day).
fn first_by_day(docs: &[Value]) -> BTreeMap<NaiveDate, &Value> {
    let mut map: BTreeMap<NaiveDate, &Value> = BTreeMap::new();
    for d in docs {
        if let Some(day) = day_of(d) {
            map.entry(day).or_insert(d);
        }
    }
    map
}

/// Per-day summary. The main period is the longest `long_sleep`; when a day has
/// none, the longest other confirmed period (not deleted, not rejected `rest`).
/// Scores come from the daily docs.
pub fn summarize(sleep: &[Value], daily_sleep: &[Value], readiness: &[Value], days: &[NaiveDate]) -> BTreeMap<NaiveDate, Night> {
    // Index each collection by day once (linear), instead of scanning every
    // document for every day: a year of nights stays fast.
    let mut periods_by_day: BTreeMap<NaiveDate, Vec<&Value>> = BTreeMap::new();
    for d in sleep.iter().filter(|d| is_confirmed_sleep(d)) {
        if let Some(day) = day_of(d) {
            periods_by_day.entry(day).or_default().push(d);
        }
    }
    let (scores, readies) = (first_by_day(daily_sleep), first_by_day(readiness));
    let mut out = BTreeMap::new();
    for &day in days {
        let periods: &[&Value] = periods_by_day.get(&day).map(Vec::as_slice).unwrap_or(&[]);
        let main = main_period(periods);
        let score = scores.get(&day).copied();
        let ready = readies.get(&day).copied();
        if main.is_none() && score.is_none() && ready.is_none() {
            continue;
        }
        let mut night = Night::default();
        if let Some(p) = main {
            night.source_id = text(p, &["id"]).map(str::to_string);
            night.period_type = text(p, &["type"]).map(str::to_string);
            night.bedtime_start = text(p, &["bedtime_start"]).and_then(parse_timestamp);
            night.bedtime_end = text(p, &["bedtime_end"]).and_then(parse_timestamp);
            night.time_in_bed_s = num(p, &["time_in_bed"]);
            night.asleep_s = num(p, &["total_sleep_duration"]);
            night.deep_s = num(p, &["deep_sleep_duration"]);
            night.rem_s = num(p, &["rem_sleep_duration"]);
            night.light_s = num(p, &["light_sleep_duration"]);
            night.awake_s = num(p, &["awake_time"]);
            night.latency_s = num(p, &["latency"]);
            night.efficiency_pct = num(p, &["efficiency"]);
            night.hr_avg_bpm = num(p, &["average_heart_rate"]);
            night.hr_lowest_bpm = num(p, &["lowest_heart_rate"]);
            night.hrv_avg_ms = num(p, &["average_hrv"]);
            night.breath_avg_per_min = num(p, &["average_breath"]);
            night.restless_periods = num(p, &["restless_periods"]);
            night.other_periods = periods.len() - 1;
        }
        night.sleep_score = score.as_ref().and_then(|d| num(d, &["score"]));
        night.readiness_score = ready.as_ref().and_then(|d| num(d, &["score"]));
        night.temp_deviation_c = ready.as_ref().and_then(|d| num(d, &["temperature_deviation"]));
        out.insert(day, night);
    }
    out
}
