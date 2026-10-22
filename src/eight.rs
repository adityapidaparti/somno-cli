//! Eight Sleep, read-only. There is no public API: this speaks the private
//! mobile-app API (`auth-api.8slp.net`, `client-api.8slp.net`) the way the
//! open-source clients do (pyEight in lukas-clarke/eight_sleep @ 1f5d84e,
//! steipete/eightctl @ 2867ce7, both checked 2026-10-20). It can change without
//! notice, and Eight Sleep rate-limits logins, so a session token is cached and
//! reused until it expires or is rejected.
//!
//! The nightly field mapping was checked against real responses (see
//! `summarize`). The open-source clients read `sleepQualityScore.*.average`,
//! which turned out to be a rolling baseline, not the night.

use anyhow::{anyhow, bail, Context, Result};
use chrono::{Duration, NaiveDate, Utc};
use reqwest::blocking::Client;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::http::{self, ApiError};
use crate::night::{day_of, num, num_any, text, Night};
// `num` and friends treat the provider's "None" strings as unknown.
use crate::log;
use crate::store;
use crate::types::parse_timestamp;

pub const AUTH_URL: &str = "https://auth-api.8slp.net/v1/tokens";
pub const API_BASE: &str = "https://client-api.8slp.net/v1/";
// The Eight Sleep mobile app's own OAuth client, as published in every
// open-source client. It identifies the app, not you; your password is the secret.
const CLIENT_ID: &str = "0894c7f33bb94800a03f1f4df13a4f38";
const CLIENT_SECRET: &str = "f0954a3ed5763ba3d06834c73731a32f15f168f47d4f164751275def86db0c76";
const SESSION_FILE: &str = "eight.json";
const CREDENTIALS_FILE: &str = "eight-credentials.json";
const LOCK_FILE: &str = "eight.lock";
const SERVICE: &str = "Eight Sleep";
pub const PASSWORD_ENV: &str = "SOMNO_EIGHT_PASSWORD";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub email: String,
    pub user_id: String,
    pub access_token: String,
    /// Unix seconds.
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub email: String,
    pub password: String,
}

pub fn load_session() -> Result<Option<Session>> {
    store::read(SESSION_FILE)
}

pub fn has_credentials() -> Result<bool> {
    Ok(store::read::<Credentials>(CREDENTIALS_FILE)?.is_some())
}

pub fn save_credentials(c: &Credentials) -> Result<std::path::PathBuf> {
    store::write(CREDENTIALS_FILE, c)
}

/// Returns whether anything was removed.
pub fn logout() -> Result<bool> {
    let _lock = store::lock(LOCK_FILE)?;
    Ok(store::remove(SESSION_FILE)? | store::remove(CREDENTIALS_FILE)?)
}

/// Validate a token response. `userId` goes into URL paths, so it must be a
/// plain identifier. Without `expires_in`, assume one hour.
pub fn session_from_response(v: &Value, email: &str, now: i64) -> Result<Session> {
    let access_token = text(v, &["access_token"]).context("Eight Sleep login response has no access token")?;
    let user_id = text(v, &["userId"]).context("Eight Sleep login response has no user id")?;
    if !user_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("Eight Sleep returned an unexpected user id format");
    }
    let expires_in = num(v, &["expires_in"]).filter(|s| *s > 0.0).unwrap_or(3600.0);
    Ok(Session {
        email: email.to_string(),
        user_id: user_id.to_string(),
        access_token: access_token.to_string(),
        // Saturating, so an absurd expiry can't overflow; capped at ten years.
        // Rounded up, so any positive expiry is later than now.
        expires_at: now.saturating_add(expires_in.min(10.0 * 365.0 * 86400.0).ceil() as i64),
    })
}

fn request_token(http: &Client, email: &str, password: &str) -> Result<Session> {
    let body = json!({
        "client_id": CLIENT_ID,
        "client_secret": CLIENT_SECRET,
        "grant_type": "password",
        "username": email,
        "password": password,
    });
    let v = http::post_json(http, SERVICE, &Url::parse(AUTH_URL).expect("static URL"), &body).map_err(|e| match e.status() {
        Some(400 | 401 | 403) => anyhow!("{e}: check the email and password"),
        Some(429) => anyhow!("{e}: logins are rate limited; wait before retrying"),
        _ => e.into(),
    })?;
    session_from_response(&v, email, Utc::now().timestamp())
}

/// Log in again with a remembered password (or `SOMNO_EIGHT_PASSWORD`).
fn relogin(http: &Client, email: &str) -> Result<Session> {
    let password = match store::read::<Credentials>(CREDENTIALS_FILE)? {
        Some(c) if c.email.eq_ignore_ascii_case(email) => c.password,
        _ => match std::env::var(PASSWORD_ENV) {
            Ok(p) if !p.is_empty() => p,
            _ => bail!("Eight Sleep session expired: run `somno eight login {email}` (add --remember to renew automatically)"),
        },
    };
    log::event("eight.relogin", format_args!("session expired or rejected; logging in again as {email}"));
    let session = request_token(http, email, &password)?;
    store::write(SESSION_FILE, &session)?;
    Ok(session)
}

fn with_hint(e: ApiError) -> anyhow::Error {
    let hint = match e.status() {
        Some(401) => ": run `somno eight login EMAIL`",
        Some(403) => ": this data may need an active Eight Sleep membership",
        Some(429) => ": rate limited, retry later",
        _ => "",
    };
    anyhow!("{e}{hint}")
}

pub struct Eight {
    http: Client,
    session: Session,
}

impl Eight {
    pub fn login(http: &Client, email: &str, password: &str) -> Result<Session> {
        let _lock = store::lock(LOCK_FILE)?;
        let session = request_token(http, email, password)?;
        store::write(SESSION_FILE, &session)?;
        Ok(session)
    }

    pub fn load(http: Client) -> Result<Self> {
        let _lock = store::lock(LOCK_FILE)?;
        let saved = load_session()?.context("not logged in to Eight Sleep: run `somno eight login EMAIL`")?;
        let session = if saved.expires_at > Utc::now().timestamp() + 60 { saved } else { relogin(&http, &saved.email)? };
        Ok(Self { http, session })
    }

    pub fn user_id(&self) -> &str {
        &self.session.user_id
    }

    fn get(&mut self, path: &str, query: &[(&str, &str)]) -> Result<Value> {
        let mut url = Url::parse(API_BASE).expect("static URL").join(path)?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let mut relogged = false;
        for attempt in 0..4u64 {
            match http::get(&self.http, SERVICE, &url, &self.session.access_token) {
                Ok(v) => return Ok(v),
                Err(e) if e.status() == Some(401) && !relogged => {
                    relogged = true;
                    let _lock = store::lock(LOCK_FILE)?;
                    // Another process may already have logged in again.
                    self.session = match load_session()? {
                        Some(s) if s.access_token != self.session.access_token && s.expires_at > Utc::now().timestamp() + 60 => s,
                        _ => relogin(&self.http, &self.session.email)?,
                    };
                }
                Err(e) if e.status() == Some(429) && attempt < 3 => {
                    log::event("eight.retry", format_args!("rate limited; retry {} after backoff", attempt + 1));
                    std::thread::sleep(std::time::Duration::from_secs(2 * (attempt + 1)));
                }
                Err(e) => return Err(with_hint(e)),
            }
        }
        bail!("Eight Sleep request failed after retries")
    }

    pub fn me(&mut self) -> Result<Value> {
        self.get("users/me", &[])
    }

    /// The trends endpoint as the app calls it: one entry per night in `days`.
    pub fn trends(&mut self, from: NaiveDate, to: NaiveDate, tz: &str) -> Result<Value> {
        let path = format!("users/{}/trends", self.session.user_id);
        let (from, to) = (from.to_string(), to.to_string());
        self.get(&path, &[
            ("tz", tz),
            ("from", &from),
            ("to", &to),
            ("include-main", "false"),
            ("include-all-sessions", "true"),
            ("model-version", "v2"),
        ])
    }

    /// Window padded by a day on each side, filtered by Eight Sleep's `day` label.
    pub fn nights(&mut self, days: &[NaiveDate], tz: &str) -> Result<BTreeMap<NaiveDate, Night>> {
        let (Some(first), Some(last)) = (days.first(), days.last()) else {
            return Ok(BTreeMap::new());
        };
        let trends = self.trends(*first - Duration::days(1), *last + Duration::days(1), tz)?;
        summarize(&trends, days)
    }
}

/// The day's main sleep session. With a `mainSessionId`, only that session;
/// if it isn't in the list, none, and the day's totals are used rather than a
/// nap's. Without one, the session with the most sleep.
fn main_session(d: &Value) -> Option<&Value> {
    let sessions = d.get("sessions")?.as_array()?;
    match text(d, &["mainSessionId"]) {
        Some(id) => sessions.iter().find(|s| text(s, &["id"]) == Some(id)),
        None => sessions.iter().rev().max_by(|a, b| {
            let sleep = |s: &Value| num(s, &["stageSummary", "sleepDuration"]).unwrap_or(-1.0);
            sleep(a).total_cmp(&sleep(b))
        }),
    }
}

/// Numeric values of a session time series (`[[timestamp, value], ...]`).
fn series(session: Option<&Value>, key: &str) -> Vec<f64> {
    session
        .and_then(|s| s.get("timeseries")?.get(key)?.as_array())
        .map(|points| points.iter().filter_map(|p| p.get(1)?.as_f64()).filter(|v| v.is_finite()).collect())
        .unwrap_or_default()
}

/// The mean, or unknown when there are no values or the sum overflows.
fn mean(xs: &[f64]) -> Option<f64> {
    (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64).filter(|m| m.is_finite())
}

/// Whether the night is still being computed: either flag set says so.
/// Unknown when neither flag is a boolean.
fn processing(d: &Value) -> Option<bool> {
    let flag = |k: &str| d.get(k).and_then(Value::as_bool);
    match (flag("processing"), flag("incomplete")) {
        (None, None) => None,
        (a, b) => Some(a == Some(true) || b == Some(true)),
    }
}

/// Per-day summary from a trends response, checked against real responses
/// (2026-10-21):
/// - Durations come from the main session's `stageSummary`, which adds up:
///   sleep + awake + out of bed = time in bed. Awake and out-of-bed time are
///   kept apart; the older "in bed minus asleep" folded them together.
/// - `sleepQualityScore.<metric>.current` is the night itself; `.average` is
///   the rolling baseline (one night's `average` equals the previous night's
///   `current`). Heart rate, HRV and breathing therefore use `current`.
/// - Lowest heart rate and bed/room temperature come from the session's
///   sample series (lowest sample; mean of samples).
///
/// Older or partial responses fall back to the day-level fields.
pub fn summarize(trends: &Value, days: &[NaiveDate]) -> Result<BTreeMap<NaiveDate, Night>> {
    let list = trends.get("days").and_then(Value::as_array).context("Eight Sleep trends response has no days list")?;
    // One entry per day is expected. If there are more, keep the longest sleep,
    // then the longest time in bed, then the larger document text, so the
    // result never depends on response order.
    let mut out: BTreeMap<NaiveDate, (Night, String)> = BTreeMap::new();
    for d in list {
        let Some(day) = day_of(d).filter(|day| days.contains(day)) else { continue };
        let session = main_session(d);
        let summary = session.and_then(|s| s.get("stageSummary")).unwrap_or(&Value::Null);
        let either = |stage_key: &str, day_key: &str| num(summary, &[stage_key]).or_else(|| num(d, &[day_key]));
        let heart = series(session, "heartRate");
        let night = Night {
            source_id: session.and_then(|s| text(s, &["id"])).map(str::to_string),
            bedtime_start: text(d, &["presenceStart"]).and_then(parse_timestamp),
            bedtime_end: text(d, &["presenceEnd"]).and_then(parse_timestamp),
            time_in_bed_s: either("totalDuration", "presenceDuration"),
            asleep_s: either("sleepDuration", "sleepDuration"),
            deep_s: either("deepDuration", "deepDuration"),
            rem_s: either("remDuration", "remDuration"),
            light_s: either("lightDuration", "lightDuration"),
            awake_s: num(summary, &["awakeDuration"]),
            out_of_bed_s: num(summary, &["outDuration"]),
            latency_s: num_any(summary, &[&["awakeBeforeSleepDuration"]])
                .or_else(|| num_any(d, &[&["latencyAsleepSeconds"], &["sleepRoutineScore", "latencyAsleepSeconds", "current"]])),
            hr_avg_bpm: num_any(d, &[&["sleepQualityScore", "heartRate", "current"], &["heartRate"]]),
            hr_lowest_bpm: heart.iter().copied().reduce(f64::min),
            hrv_avg_ms: num_any(d, &[&["sleepQualityScore", "hrv", "current"], &["hrv"]]),
            breath_avg_per_min: num_any(d, &[&["sleepQualityScore", "respiratoryRate", "current"], &["respiratoryRate"]]),
            sleep_score: num_any(d, &[&["score"], &["sleepFitnessScore", "total"]]),
            bed_temp_c: mean(&series(session, "tempBedC")),
            room_temp_c: mean(&series(session, "tempRoomC")),
            toss_turns: num(d, &["tnt"]),
            processing: processing(d),
            ..Night::default()
        };
        let raw = d.to_string();
        let rank = |n: &Night, raw: &str| (n.asleep_s.unwrap_or(-1.0), n.time_in_bed_s.unwrap_or(-1.0), raw.to_string());
        let keep = out.get(&day).is_none_or(|(old, old_raw)| {
            let (a, b) = (rank(old, old_raw), rank(&night, &raw));
            a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2)).is_lt()
        });
        if keep {
            out.insert(day, (night, raw));
        }
    }
    Ok(out.into_iter().map(|(day, (night, _))| (day, night)).collect())
}
