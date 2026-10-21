//! Example tests over synthetic fixtures: summaries, auth parsing, pagination,
//! the destination allowlist and rendering. No network.

use chrono::NaiveDate;
use chrono_tz::Tz;
use serde_json::{json, Value};
use somno::eight;
use somno::http::{check_destination, short_message};
use somno::night::{render_night, render_table, slot, Night, Slot, Source};
use somno::oura::{self, authorize_url, paginate, parse_callback, tokens_from_response, window_params, Callback, Tokens};
use somno::types::{days_inclusive, fmt_duration, DaySpec};

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn data(name: &str) -> Vec<Value> {
    fixture(name)["data"].as_array().unwrap().clone()
}

fn d(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn ny() -> Tz {
    "America/New_York".parse().unwrap()
}

fn oura_nights() -> std::collections::BTreeMap<NaiveDate, Night> {
    oura::summarize(
        &data("oura_sleep.json"),
        &data("oura_daily_sleep.json"),
        &data("oura_daily_readiness.json"),
        &days_inclusive(d("2026-10-18"), d("2026-10-21")),
    )
}

// ---- Oura summaries ----

#[test]
fn oura_main_period_fields_and_daily_scores() {
    let nights = oura_nights();
    let n = &nights[&d("2026-10-18")];
    assert_eq!(n.source_id.as_deref(), Some("s-oct18-main"));
    assert_eq!(n.asleep_s, Some(24720.0));
    assert_eq!(n.deep_s, Some(4200.0));
    assert_eq!(n.hr_lowest_bpm, Some(48.0));
    assert_eq!(n.hrv_avg_ms, Some(62.0));
    assert_eq!(n.breath_avg_per_min, Some(14.625));
    assert_eq!(n.sleep_score, Some(82.0));
    assert_eq!(n.readiness_score, Some(79.0));
    assert_eq!(n.temp_deviation_c, Some(0.12));
    assert_eq!(n.other_periods, 1, "the late nap is counted, not merged");
    assert_eq!(n.bedtime_start.unwrap().to_rfc3339(), "2026-10-17T23:41:00-04:00");
}

#[test]
fn oura_picks_longest_long_sleep_and_ignores_deleted() {
    let nights = oura_nights();
    let n = &nights[&d("2026-10-19")];
    assert_eq!(n.source_id.as_deref(), Some("s-oct19-long"));
    assert_eq!(n.hrv_avg_ms, None, "null is unknown, not zero");
    assert_eq!(n.sleep_score, None);
    assert_eq!(n.other_periods, 1, "deleted period is not counted");
}

#[test]
fn oura_falls_back_to_other_period_types_and_keeps_score_only_days() {
    let nights = oura_nights();
    assert_eq!(nights[&d("2026-10-20")].period_type.as_deref(), Some("sleep"));
    let scores_only = &nights[&d("2026-10-21")];
    assert_eq!(scores_only.asleep_s, None);
    assert_eq!(scores_only.sleep_score, Some(70.0));
    assert_eq!(scores_only.temp_deviation_c, Some(-0.3));
    assert!(!nights.contains_key(&d("2026-10-10")), "days outside the request are dropped");
}

// ---- Eight Sleep summaries ----

#[test]
fn eight_reads_the_main_session_and_tonights_values() {
    let nights = eight::summarize(&fixture("eight_trends.json"), &days_inclusive(d("2026-10-18"), d("2026-10-19"))).unwrap();
    let n = &nights[&d("2026-10-18")];
    assert_eq!(n.source_id.as_deref(), Some("s-main"), "mainSessionId, not the first (nap) session");
    assert_eq!(n.asleep_s, Some(24000.0));
    assert_eq!(n.time_in_bed_s, Some(27420.0));
    assert_eq!(n.awake_s, Some(2400.0), "stageSummary awake, not in-bed minus asleep");
    assert_eq!(n.out_of_bed_s, Some(1020.0));
    assert_eq!(n.latency_s, Some(600.0));
    assert_eq!(n.hrv_avg_ms, Some(58.0), "current is the night; average is the baseline");
    assert_eq!(n.hr_avg_bpm, Some(55.4));
    assert_eq!(n.breath_avg_per_min, Some(14.2));
    assert_eq!(n.hr_lowest_bpm, Some(49.0), "lowest numeric sample of the main session");
    assert_eq!(n.bed_temp_c, Some(31.0));
    assert_eq!(n.room_temp_c, Some(20.5));
    assert_eq!(n.toss_turns, Some(24.0));
    assert_eq!(n.sleep_score, Some(85.0));
    assert_eq!(n.processing, Some(false), "from `incomplete`");
    assert_eq!(n.bedtime_start.unwrap().with_timezone(&ny()).format("%H:%M").to_string(), "23:38");
}

#[test]
fn eight_day_level_fallbacks_and_none_strings() {
    let nights = eight::summarize(&fixture("eight_trends.json"), &[d("2026-10-19")]).unwrap();
    let n = &nights[&d("2026-10-19")];
    assert_eq!(n.asleep_s, None);
    assert_eq!(n.time_in_bed_s, Some(20000.0));
    assert_eq!(n.awake_s, None, "no stageSummary: awake is unknown, not in-bed minus asleep");
    assert_eq!(n.out_of_bed_s, None);
    assert_eq!(n.bedtime_end, None);
    assert_eq!(n.sleep_score, Some(77.0), "\"None\" score falls back to sleepFitnessScore.total");
    assert_eq!(n.hr_avg_bpm, Some(57.0));
    assert_eq!(n.breath_avg_per_min, Some(15.1));
    assert_eq!(n.latency_s, Some(900.0));
    assert_eq!(n.hrv_avg_ms, None, "a lone baseline average is not the night's HRV");
    assert_eq!(n.processing, Some(true));
}

#[test]
fn eight_rejects_response_without_days() {
    assert!(eight::summarize(&json!({"error": "x"}), &[d("2026-10-18")]).is_err());
}

#[test]
fn eight_session_validation() {
    let ok = eight::session_from_response(&json!({"access_token": "t", "expires_in": 72000, "userId": "abc-123"}), "a@b.c", 1000).unwrap();
    assert_eq!(ok.expires_at, 73000);
    assert_eq!(ok.user_id, "abc-123");
    let default_expiry = eight::session_from_response(&json!({"access_token": "t", "userId": "u1"}), "a@b.c", 0).unwrap();
    assert_eq!(default_expiry.expires_at, 3600);
    assert!(eight::session_from_response(&json!({"access_token": "t", "userId": "../admin"}), "a@b.c", 0).is_err());
    assert!(eight::session_from_response(&json!({"userId": "u1"}), "a@b.c", 0).is_err());
}

// ---- Oura auth ----

#[test]
fn authorize_url_has_exact_redirect_and_every_scope() {
    let url = reqwest::Url::parse(&authorize_url("client-1", "st8")).unwrap();
    let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(url.host_str(), Some("cloud.ouraring.com"));
    assert_eq!(q["redirect_uri"], "http://localhost:8765/callback");
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["state"], "st8");
    // The eleven scopes on the developer portal's app form (2026-10-20).
    let mut scopes: Vec<&str> = q["scope"].split(' ').collect();
    scopes.sort();
    assert_eq!(scopes, vec!["daily", "email", "heart_health", "heartrate", "personal", "ring_configuration", "session", "spo2", "stress", "tag", "workout"]);
}

#[test]
fn missing_scopes_against_grant() {
    use somno::oura::missing_scopes;
    assert_eq!(missing_scopes(None), None, "unknown when Oura doesn't say");
    assert_eq!(missing_scopes(Some(&oura::SCOPES.join(" "))), Some(vec![]));
    // As Oura actually reports them (2026-10-20):
    let prefixed: Vec<String> = oura::SCOPES.iter().map(|s| format!("extapi:{s}")).collect();
    assert_eq!(missing_scopes(Some(&prefixed.join(" "))), Some(vec![]));
    assert_eq!(missing_scopes(Some("extapi:daily extapi:email")).unwrap().len(), 9);
    assert_eq!(
        missing_scopes(Some("email personal daily heartrate workout tag session spo2 ring_configuration")),
        Some(vec!["stress", "heart_health"])
    );
}

#[test]
fn configure_check_verdicts() {
    use somno::oura::{interpret_authorize, interpret_token_probe, Check, ClientProbe};
    assert!(matches!(interpret_authorize(302), Check::Pass(_)), "sent on to sign-in");
    assert!(matches!(interpret_authorize(200), Check::Pass(_)));
    assert!(matches!(interpret_authorize(400), Check::Fail(m) if m.contains("http://localhost:8765/callback")));
    assert!(matches!(interpret_authorize(503), Check::Unknown(_)));
    // Answers observed from Oura on 2026-10-20 for a wrong secret:
    assert_eq!(interpret_token_probe(401, Some(&json!({"error": "invalid_client"}))), ClientProbe::Rejected, "moi.ouraring.com");
    assert_eq!(
        interpret_token_probe(400, Some(&json!({"status": 400, "error": "invalid_client", "error_description": "Invalid client_id."}))),
        ClientProbe::Rejected,
        "api.ouraring.com, unknown client"
    );
    // Right credentials, fake code (RFC 6749 §5.2):
    assert_eq!(interpret_token_probe(400, Some(&json!({"error": "invalid_grant"}))), ClientProbe::Accepted);
    assert!(matches!(interpret_token_probe(400, Some(&json!({"error": "invalid_request"}))), ClientProbe::Unclear(_)));
    assert!(matches!(interpret_token_probe(500, None), ClientProbe::Unclear(_)));
}

#[test]
fn callback_validation() {
    let state = "abc";
    assert_eq!(parse_callback("/callback?code=xyz&state=abc", state), Callback::Code("xyz".into()));
    assert_eq!(parse_callback("/callback?code=a%2Fb&state=abc", state), Callback::Code("a/b".into()));
    assert_eq!(parse_callback("/callback?error=access_denied&state=abc", state), Callback::Denied("access_denied".into()));
    assert!(matches!(parse_callback("/callback?code=xyz&state=abd", state), Callback::Invalid(_)));
    assert!(matches!(parse_callback("/callback?code=xyz", state), Callback::Invalid(_)));
    assert!(matches!(parse_callback("/favicon.ico", state), Callback::Invalid(_)));
    assert!(matches!(parse_callback("/callback?code=1&code=2&state=abc", state), Callback::Invalid(_)));
    assert!(matches!(parse_callback("/callback?state=abc", state), Callback::Invalid(_)));
    assert!(matches!(parse_callback("/callback?code=x&state=", ""), Callback::Invalid(_)), "empty expected state never matches");
    // An attacker's error with a wrong state must not abort the login.
    assert!(matches!(parse_callback("/callback?error=x&state=nope", state), Callback::Invalid(_)));
}

#[test]
fn pasted_redirect_forms_and_validation() {
    use somno::oura::parse_pasted;
    let state = "abc";
    let code = Callback::Code("xyz".into());
    assert_eq!(parse_pasted("http://localhost:8765/callback?code=xyz&state=abc", state), code);
    assert_eq!(parse_pasted("  \"http://localhost:8765/callback?code=xyz&state=abc&scope=daily\"\n", state), code);
    assert_eq!(parse_pasted("localhost:8765/callback?code=xyz&state=abc", state), code, "address bar without scheme");
    assert_eq!(parse_pasted("/callback?code=xyz&state=abc", state), code);
    assert_eq!(parse_pasted("?code=xyz&state=abc", state), code);
    assert_eq!(parse_pasted("code=xyz&state=abc", state), code);
    assert_eq!(parse_pasted("http://localhost:8765/callback?error=access_denied&state=abc", state), Callback::Denied("access_denied".into()));
    assert!(matches!(parse_pasted("http://localhost:8765/callback?code=xyz&state=old", state), Callback::Invalid(_)), "state from another attempt");
    assert!(matches!(parse_pasted("https://evil.example:8765/callback?code=xyz&state=abc", state), Callback::Invalid(_)));
    assert!(matches!(parse_pasted("http://localhost:9999/callback?code=xyz&state=abc", state), Callback::Invalid(_)));
    assert!(matches!(parse_pasted("http://localhost:8765/other?code=xyz&state=abc", state), Callback::Invalid(_)));
    assert!(matches!(parse_pasted("xyz", state), Callback::Invalid(_)), "a bare code has no state to check");
}

#[test]
fn token_response_validation_and_rotation() {
    let first = tokens_from_response(&json!({"access_token": "a1", "refresh_token": "r1", "expires_in": 86400, "token_type": "bearer", "scope": "daily"}), 100, None).unwrap();
    assert_eq!(first, Tokens { access_token: "a1".into(), refresh_token: "r1".into(), expires_at: 86500, scope: Some("daily".into()) });
    let rotated = tokens_from_response(&json!({"access_token": "a2", "expires_in": 60}), 200, Some(&first)).unwrap();
    assert_eq!(rotated.refresh_token, "r1", "no new refresh token keeps the previous one");
    assert_eq!(rotated.scope.as_deref(), Some("daily"));
    assert!(tokens_from_response(&json!({"access_token": "a", "refresh_token": "r"}), 0, None).is_err(), "missing expiry");
    assert!(tokens_from_response(&json!({"access_token": "a", "refresh_token": "r", "expires_in": 0}), 0, None).is_err());
    assert!(tokens_from_response(&json!({"access_token": "a", "refresh_token": "r", "expires_in": 9, "token_type": "mac"}), 0, None).is_err());
    assert!(tokens_from_response(&json!({"access_token": "a", "expires_in": 9}), 0, None).is_err(), "first login needs a refresh token");
}

// ---- Oura pagination and query windows ----

#[test]
fn paginate_follows_tokens_and_merges_pages() {
    let pages = [
        json!({"data": [{"n": 1}, {"n": 2}], "next_token": "p2"}),
        json!({"data": [{"n": 3}], "next_token": "p3"}),
        json!({"data": [], "next_token": null}),
    ];
    let mut asked = Vec::new();
    let docs = paginate(|token| {
        asked.push(token.map(str::to_string));
        Ok(pages[asked.len() - 1].clone())
    })
    .unwrap();
    assert_eq!(docs.len(), 3);
    assert_eq!(asked, vec![None, Some("p2".into()), Some("p3".into())]);
}

#[test]
fn paginate_rejects_loops_and_bad_pages() {
    assert!(paginate(|_| Ok(json!({"data": [], "next_token": "same"}))).is_err());
    assert!(paginate(|_| Ok(json!({"next_token": null}))).is_err());
    assert!(paginate(|_| Ok(json!({"data": [], "next_token": 5}))).is_err());
}

#[test]
fn window_params_by_collection_kind_and_dst() {
    let tz = ny();
    let (s, e) = (Some(d("2026-03-07")), Some(d("2026-03-09")));
    assert_eq!(
        window_params("sleep", s, e, tz).unwrap(),
        vec![("start_date".into(), "2026-03-07".into()), ("end_date".into(), "2026-03-09".into())]
    );
    assert_eq!(
        window_params("heartrate", s, e, tz).unwrap(),
        vec![
            ("start_datetime".into(), "2026-03-07T00:00:00-05:00".into()),
            ("end_datetime".into(), "2026-03-09T00:00:00-04:00".into()),
        ],
        "offset follows the DST change on 2026-03-08"
    );
    assert!(window_params("personal_info", s, e, tz).unwrap().is_empty());
}

// ---- network allowlist and error text ----

#[test]
fn destination_allowlist() {
    let ok = |u: &str| check_destination(&reqwest::Url::parse(u).unwrap()).is_ok();
    assert!(ok("https://api.ouraring.com/v2/usercollection/sleep"));
    assert!(ok(somno::oura::TOKEN_URL));
    assert!(ok(somno::oura::TOKEN_URL_LEGACY));
    assert!(ok(somno::oura::AUTHORIZE_URL));
    assert!(ok("https://client-api.8slp.net/v1/users/me"));
    assert!(ok("https://auth-api.8slp.net/v1/tokens"));
    assert!(!ok("http://api.ouraring.com/v2/usercollection/sleep"), "plain HTTP");
    assert!(!ok("https://api.ouraring.com.evil.example/"), "suffix host");
    assert!(!ok("https://api.ouraring.com:8443/"), "explicit port");
    assert!(!ok("https://user@api.ouraring.com/"), "userinfo");
    assert!(!ok("https://app-api.8slp.net/v1/users/x/temperature"), "Pod control host is not allowed");
}

#[test]
fn short_message_extracts_only_short_text() {
    assert_eq!(short_message(br#"{"error":"invalid_grant"}"#).as_deref(), Some("invalid_grant"));
    assert_eq!(short_message(br#"{"detail":[{"loc":["q"]}]}"#), None, "structured detail is dropped");
    assert_eq!(short_message(b"<html>oops</html>"), None);
    assert_eq!(short_message(format!(r#"{{"message":"{}"}}"#, "x".repeat(500)).as_bytes()).unwrap().len(), 160);
}

// ---- rendering ----

#[test]
fn render_night_side_by_side_with_unknowns_and_unavailable() {
    let oura = oura_nights();
    let eight = eight::summarize(&fixture("eight_trends.json"), &[d("2026-10-18")]).unwrap();
    let day = d("2026-10-18");
    let columns = vec![
        (Source::Oura, slot(&Ok(oura), day)),
        (Source::EightSleep, slot(&Ok(eight), day)),
    ];
    let out = render_night(day, ny(), &columns);
    assert!(out.starts_with("Night ending Sun 2026-10-18 (America/New_York)\n"), "{out}");
    let line = |label: &str| out.lines().find(|l| l.starts_with(label)).unwrap_or_else(|| panic!("no {label} row in\n{out}")).to_string();
    assert!(line("in bed ").contains("23:41 → 07:12"));
    assert!(line("in bed ").contains("23:38 → 07:15"));
    assert!(line("asleep").contains("6h 52m") && line("asleep").contains("6h 40m"));
    assert!(line("readiness").contains("79") && line("readiness").contains('—'));
    assert!(line("note").contains("+1 other period(s)"));
    assert!(!out.contains("still processing"));

    let out = render_night(day, ny(), &[(Source::EightSleep, Slot::Unavailable { reason: "not logged in".into() })]);
    assert!(out.contains("unavailable"));
    assert!(out.ends_with("Eight Sleep: not logged in\n"));
    assert!(!out.contains("asleep"), "rows with no value in any column are omitted");
}

#[test]
fn render_table_rows_per_source_and_errors_at_bottom() {
    let days = days_inclusive(d("2026-10-18"), d("2026-10-19"));
    let out = render_table(&days, ny(), &[(Source::Oura, Ok(oura_nights())), (Source::EightSleep, Err("session expired".into()))]);
    assert!(out.contains("2026-10-18  Oura"));
    assert!(out.contains("6h 52m"));
    assert!(!out.contains("Eight Sleep  "), "unavailable source has no rows");
    assert!(out.ends_with("Eight Sleep: session expired\n"));
}

// ---- small helpers ----

#[test]
fn day_specs_and_durations() {
    let today = d("2026-10-20");
    assert_eq!("yesterday".parse::<DaySpec>().unwrap().resolve(today), d("2026-10-19"));
    assert_eq!("-7".parse::<DaySpec>().unwrap().resolve(today), d("2026-10-13"));
    assert_eq!("2026-01-02".parse::<DaySpec>().unwrap().resolve(today), d("2026-01-02"));
    assert!("tomorrow".parse::<DaySpec>().is_err());
    assert_eq!(fmt_duration(24720.0).as_deref(), Some("6h 52m"));
    assert_eq!(fmt_duration(3600.0).as_deref(), Some("1h 00m"));
    assert_eq!(fmt_duration(2340.0).as_deref(), Some("39m"));
    assert_eq!(fmt_duration(-1.0), None);
    assert_eq!(fmt_duration(f64::NAN), None);
}

#[test]
fn log_urls_redact_credentials_and_oauth_values() {
    let url = reqwest::Url::parse(
        "https://cloud.ouraring.com/oauth/authorize?response_type=code&client_id=abc-123&redirect_uri=http%3A%2F%2Flocalhost%3A8765%2Fcallback&scope=daily+tag&state=s3cr3t&code=c0de",
    )
    .unwrap();
    let logged = somno::log::redact_url(&url);
    for secret in ["abc-123", "s3cr3t", "c0de"] {
        assert!(!logged.contains(secret), "{secret} leaked into {logged}");
    }
    assert!(logged.contains("redirect_uri=http%3A%2F%2Flocalhost%3A8765%2Fcallback"), "{logged}");
    assert!(logged.contains("scope=daily+tag"));
    let dated = reqwest::Url::parse("https://api.ouraring.com/v2/usercollection/sleep?start_date=2026-10-17&end_date=2026-10-21&next_token=tok").unwrap();
    assert_eq!(
        somno::log::redact_url(&dated),
        "https://api.ouraring.com/v2/usercollection/sleep?start_date=2026-10-17&end_date=2026-10-21&next_token=%E2%80%A6"
    );
}

#[test]
fn oura_times_stay_local_when_traveling() {
    // A night in Europe (+02:00) viewed with New York as the display zone.
    let doc = json!({"id": "abroad", "day": "2026-03-10", "type": "long_sleep",
        "bedtime_start": "2026-03-10T02:30:00.000+02:00", "bedtime_end": "2026-03-10T09:45:00.000+02:00",
        "total_sleep_duration": 21000, "time_in_bed": 26100});
    let day = d("2026-03-10");
    let nights = oura::summarize(&[doc], &[], &[], &[day]);
    let out = render_night(day, ny(), &[(Source::Oura, slot(&Ok(nights.clone()), day))]);
    assert!(out.contains("02:30 → 09:45 (+02:00)"), "{out}");
    let table = render_table(&[day], ny(), &[(Source::Oura, Ok(nights))]);
    assert!(table.contains("02:30 → 09:45 (+02:00)"), "{table}");
}
