//! Oura data views: day selection, formatting, tables, details, heart rate,
//! and the day dashboard. Synthetic documents shaped like Oura OpenAPI 1.42.

use chrono::NaiveDate;
use chrono_tz::Tz;
use serde_json::{json, Value};
use somno::views::{self, doc_day, hr_buckets, in_range, render_detail, render_table, sort_docs, thousands};
use std::collections::BTreeMap;

fn d(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn ny() -> Tz {
    "America/New_York".parse().unwrap()
}

#[test]
fn every_command_has_a_view_and_ring_hardware_stays_raw() {
    let names: Vec<&str> = views::views().iter().map(|v| v.name).collect();
    for n in ["readiness", "sleep", "sleep-score", "activity", "stress", "resilience", "spo2", "cardio-age", "vo2max", "workouts", "sessions", "tags", "bedtime", "rest-mode"] {
        assert!(names.contains(&n), "missing view {n}");
    }
    let covered: Vec<&str> = views::views().iter().flat_map(|v| v.collections.iter().copied()).collect();
    for c in somno::oura::COLLECTIONS {
        let elsewhere = matches!(*c, "heartrate" | "personal_info" | "ring_configuration" | "ring_battery_level");
        assert!(covered.contains(c) || elsewhere, "collection {c} has no view");
    }
}

#[test]
fn day_selection_handles_days_and_spans() {
    let (s, e) = (d("2026-10-14"), d("2026-10-16"));
    assert!(in_range("sleep", &json!({"day": "2026-10-15"}), s, e));
    assert!(!in_range("sleep", &json!({"day": "2026-10-13"}), s, e), "padding day is dropped");
    assert!(!in_range("sleep", &json!({"day": "2026-10-17"}), s, e));
    // Spans count when they overlap.
    assert!(in_range("rest_mode_period", &json!({"start_day": "2026-10-01", "end_day": "2026-10-14"}), s, e));
    assert!(!in_range("rest_mode_period", &json!({"start_day": "2026-10-01", "end_day": "2026-10-13"}), s, e));
    // No end day: rest mode is still going; an enhanced tag has no duration.
    assert!(in_range("rest_mode_period", &json!({"start_day": "2026-10-10", "end_day": null}), s, e));
    assert!(!in_range("enhanced_tag", &json!({"start_day": "2026-10-10", "end_day": null}), s, e));
    assert!(in_range("enhanced_tag", &json!({"start_day": "2026-10-15", "end_day": null}), s, e));
    assert!(!in_range("sleep", &json!({"text": "no day"}), s, e));
    assert!(!in_range("rest_mode_period", &json!({"start_day": "2026-10-10", "end_day": null}), e, s), "a reversed range is empty");
    assert_eq!(doc_day(&json!({"start_day": "2026-10-15"})), Some(d("2026-10-15")));
}

#[test]
fn docs_sort_by_day_then_their_own_start_time() {
    let mut docs = vec![
        json!({"day": "2026-10-19", "bedtime_start": "2026-10-19T13:00:00+01:00"}),
        json!({"day": "2026-10-18", "start_datetime": "2026-10-18T07:00:00-04:00"}),
        // Earlier in absolute time despite the later clock reading:
        json!({"day": "2026-10-19", "bedtime_start": "2026-10-19T01:30:00+02:00"}),
    ];
    sort_docs(&mut docs);
    let order: Vec<&str> = docs.iter().map(|d| d["bedtime_start"].as_str().or(d["start_datetime"].as_str()).unwrap()).collect();
    assert_eq!(order, ["2026-10-18T07:00:00-04:00", "2026-10-19T01:30:00+02:00", "2026-10-19T13:00:00+01:00"]);
}

#[test]
fn thousands_separators() {
    assert_eq!(thousands(0.0), "0");
    assert_eq!(thousands(312.0), "312");
    assert_eq!(thousands(12345.0), "12,345");
    assert_eq!(thousands(1234567.0), "1,234,567");
    assert_eq!(thousands(-2500.0), "-2,500");
}

#[test]
fn activity_table_formats_units_and_unknowns() {
    let view = views::view("activity").unwrap();
    let docs = vec![
        json!({"day": "2026-10-14", "score": 88, "steps": 10422, "active_calories": 610, "total_calories": 2940,
               "high_activity_time": 0, "medium_activity_time": 3000, "low_activity_time": 20400, "sedentary_time": 28800}),
        json!({"day": "2026-10-15", "score": null}),
    ];
    let out = render_table(&view, &docs);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("day         score"));
    assert!(lines[1].contains("10,422") && lines[1].contains("2,940") && lines[1].contains("50m") && lines[1].contains("8h 00m"), "{}", lines[1]);
    assert!(lines[2].starts_with("2026-10-15") && lines[2].contains('—'), "missing values show as unknown: {}", lines[2]);
}

#[test]
fn detail_shows_every_field_with_units() {
    let doc = json!({
        "id": "hidden", "day": "2026-03-10", "type": "late_nap", "time_in_bed": 6300, "total_sleep_duration": 4800,
        "bedtime_start": "2026-03-09T17:40:00.000+02:00", "efficiency": 75, "average_breath": 15.875,
        "lowest_heart_rate": 61, "average_hrv": 63, "low_battery_alert": false, "ring_id": null,
        "readiness": {"score": 80, "contributors": {"hrv_balance": 92, "body_temperature": null}},
        "heart_rate": {"interval": 300, "items": [61, null, 68, 63], "timestamp": "2026-03-09T17:40:00+02:00"},
        "movement_30_sec": "1".repeat(214),
        "some_new_field": 12.345,
    });
    let out = render_detail(views::view("sleep").unwrap().first, &doc, ny());
    let line = |label: &str| out.lines().find(|l| l.trim_start().starts_with(label)).unwrap_or_else(|| panic!("no {label} in\n{out}")).to_string();
    assert!(!out.contains("hidden"), "ids are not shown");
    assert!(line("type").ends_with("late nap"));
    assert!(line("time in bed").ends_with("1h 45m"));
    assert!(line("total sleep duration").ends_with("1h 20m"));
    assert!(line("bedtime start").ends_with("2026-03-09 17:40 (+02:00)"), "local time abroad, with offset");
    assert!(line("efficiency").ends_with("75%"));
    assert!(line("average breath").ends_with("15.9 /min"));
    assert!(line("lowest heart rate").ends_with("61 bpm"));
    assert!(line("average hrv").ends_with("63 ms"));
    assert!(line("low battery alert").ends_with("no"));
    assert!(line("ring id").ends_with('—'));
    assert!(line("hrv balance").ends_with("92 / 100"), "nested contributors are scores out of 100");
    assert!(line("body temperature").ends_with('—'));
    assert!(line("heart rate").contains("3 samples every 300s") && line("heart rate").contains("avg 64 bpm"), "{}", line("heart rate"));
    assert!(line("movement 30 sec").contains("214 coded epochs; decoded by `somno stages`"), "{}", line("movement 30 sec"));
    assert!(line("some new field").ends_with("12.35"), "unknown fields still appear");
    // Curated fields come first, in the view's order.
    let day_at = out.find("  day ").unwrap();
    let type_at = out.find("  type ").unwrap();
    let new_at = out.find("some new field").unwrap();
    assert!(day_at < type_at && type_at < new_at);
}

#[test]
fn bedtime_window_from_offsets() {
    let doc = json!({"day": "2026-10-20", "optimal_bedtime": {"day_tz": -14400, "start_offset": -3600, "end_offset": 1800},
                     "recommendation": "follow_optimal_bedtime", "status": "optimal_found"});
    let view = views::view("bedtime").unwrap();
    let table = render_table(&view, std::slice::from_ref(&doc));
    assert!(table.contains("23:00–00:30"), "{table}");
    assert!(table.contains("follow optimal bedtime"));
    assert!(render_detail(view.first, &doc, ny()).contains("23:00–00:30 (local)"));
}

#[test]
fn tags_read_both_tag_collections() {
    let view = views::view("tags").unwrap();
    let docs = vec![
        json!({"start_day": "2026-10-20", "start_time": "2026-10-20T21:00:00-04:00", "end_day": null, "end_time": null,
               "tag_type_code": "tag_generic_caffeine", "custom_name": null, "comment": "late coffee"}),
        json!({"start_day": "2026-10-20", "start_time": "2026-10-20T22:00:00-04:00", "tag_type_code": "custom", "custom_name": "sauna"}),
        json!({"day": "2026-10-20", "timestamp": "2026-10-20T08:00:00-04:00", "tags": ["alcohol", "late_meal"], "text": "dinner out"}),
    ];
    let out = render_table(&view, &docs);
    assert!(out.contains("21:00") && out.contains("caffeine") && out.contains("late coffee"), "{out}");
    assert!(out.contains("sauna"));
    assert!(out.contains("alcohol, late_meal") && out.contains("dinner out"));
}

#[test]
fn workouts_and_sessions_derive_duration_and_sample_means() {
    let w = json!({"day": "2026-10-20", "activity": "running", "intensity": "moderate", "calories": 320.4, "distance": 6200.0,
                   "start_datetime": "2026-10-20T07:10:00-04:00", "end_datetime": "2026-10-20T07:55:00-04:00", "source": "autodetected"});
    let out = render_table(&views::view("workouts").unwrap(), &[w]);
    assert!(out.contains("07:10–07:55") && out.contains("45m") && out.contains("320") && out.contains("6.2 km"), "{out}");
    let s = json!({"day": "2026-10-20", "type": "meditation", "mood": "good",
                   "start_datetime": "2026-10-20T12:30:00-04:00", "end_datetime": "2026-10-20T12:40:00-04:00",
                   "heart_rate": {"interval": 5, "items": [60, 62, null, 64]}, "heart_rate_variability": {"interval": 5, "items": [40, 50]}});
    let out = render_table(&views::view("sessions").unwrap(), &[s]);
    assert!(out.contains("12:30–12:40") && out.contains("10m") && out.contains("good") && out.contains("62") && out.contains("45"), "{out}");
}

#[test]
fn heart_rate_buckets_by_local_day_and_hour() {
    let samples = vec![
        json!({"bpm": 60, "source": "rest", "timestamp": "2026-10-20T03:30:00+00:00"}),  // 23:30 on the 19th in New York
        json!({"bpm": 80, "source": "awake", "timestamp": "2026-10-20T13:00:00+00:00"}), // 09:00 on the 20th
        json!({"bpm": 100, "source": "workout", "timestamp": "2026-10-20T13:30:00+00:00"}),
        json!({"bpm": null, "source": "awake", "timestamp": "2026-10-20T14:00:00+00:00"}),
    ];
    let days = hr_buckets(&samples, ny(), false);
    assert_eq!(days.len(), 2);
    let today = &days[&(d("2026-10-20"), None)];
    assert_eq!((today.samples, today.min, today.max, today.avg()), (2, 80.0, 100.0, Some(90.0)));
    assert_eq!(today.by_source.get("workout"), Some(&1));
    let hours = hr_buckets(&samples, ny(), true);
    let hour = |s: &str| (d(&s[..10]), Some(chrono::DateTime::parse_from_rfc3339(s).unwrap()));
    assert!(hours.contains_key(&hour("2026-10-19T23:00:00-04:00")));
    assert_eq!(hours[&hour("2026-10-20T09:00:00-04:00")].samples, 2);
    let table = views::render_hr(&hours);
    assert!(table.lines().nth(1).unwrap().starts_with("23:00 "), "{table}");
}

#[test]
fn heart_rate_keeps_both_hours_when_clocks_fall_back() {
    // 2026-11-01 01:30 happens twice in New York: at -04:00, then at -05:00.
    let samples = vec![
        json!({"bpm": 50, "source": "sleep", "timestamp": "2026-11-01T05:30:00+00:00"}),
        json!({"bpm": 70, "source": "sleep", "timestamp": "2026-11-01T06:30:00+00:00"}),
    ];
    let hours = hr_buckets(&samples, ny(), true);
    assert_eq!(hours.len(), 2);
    let table = views::render_hr(&hours);
    assert!(table.contains("01:00 (-04:00)") && table.contains("01:00 (-05:00)"), "{table}");
}

#[test]
fn day_dashboard_lines_and_unknowns() {
    let mut docs: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    docs.insert("daily_readiness".into(), vec![json!({"day": "2026-10-20", "score": 71, "temperature_deviation": 0.21})]);
    docs.insert("daily_sleep".into(), vec![json!({"day": "2026-10-20", "score": 80})]);
    docs.insert("sleep".into(), vec![
        json!({"day": "2026-10-20", "type": "long_sleep", "bedtime_start": "2026-10-19T23:05:00-04:00", "bedtime_end": "2026-10-20T06:50:00-04:00",
               "total_sleep_duration": 25200, "deep_sleep_duration": 5400, "rem_sleep_duration": 6000}),
        json!({"day": "2026-10-20", "type": "late_nap", "total_sleep_duration": 1200}),
    ]);
    docs.insert("daily_activity".into(), vec![json!({"day": "2026-10-20", "score": 58, "steps": 9481, "active_calories": 512, "total_calories": 2604})]);
    docs.insert("daily_stress".into(), vec![json!({"day": "2026-10-20", "day_summary": "normal", "stress_high": 3600, "recovery_high": 2700})]);
    docs.insert("workout".into(), vec![]);
    let errors = BTreeMap::from([("vO2_max".to_string(), "Oura HTTP 403".to_string())]);
    let out = views::render_day(d("2026-10-20"), ny(), &docs, &errors);
    let line = |label: &str| out.lines().find(|l| l.starts_with(label)).unwrap_or_else(|| panic!("no {label} in\n{out}")).to_string();
    assert!(line("readiness").contains("71 · temp +0.21 °C"));
    assert!(line("sleep").contains("80 · 23:05–06:50, 7h 00m asleep (deep 1h 30m, REM 1h 40m) · +1 other period(s)"), "{}", line("sleep"));
    assert!(line("activity").contains("9,481 steps · 512 active / 2,604 total kcal"));
    assert!(line("stress").contains("normal · high stress 1h 00m · high recovery 45m"));
    assert!(line("resilience").ends_with('—'), "no document is unknown, not empty");
    assert!(line("workouts").ends_with("none"));
    assert!(out.ends_with("vO2_max: Oura HTTP 403\n"));
}

#[test]
fn empty_heart_rate_bucket_has_no_average() {
    assert_eq!(views::HrStats::default().avg(), None);
}
