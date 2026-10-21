//! Sleep-stage decoding from Oura's phase strings: epochs, runs, totals,
//! onset and awakenings, movement, the chart and the JSON form.

use serde_json::{json, Value};
use somno::stages::{events, movement, period_json, render_chart, render_period, stage_stats, Hypnogram, Stage};

/// 30-second codes: 4 min awake, 5 min light, 6 min deep, 1 min awake,
/// 3 min light, 5 min REM, 6 min awake (30 min in bed).
fn night() -> Value {
    let codes = format!("{}{}{}{}{}{}{}", "4".repeat(8), "2".repeat(10), "1".repeat(12), "4".repeat(2), "2".repeat(6), "3".repeat(10), "4".repeat(12));
    json!({
        "id": "n1", "day": "2026-10-21", "type": "long_sleep",
        "bedtime_start": "2026-10-20T23:00:00-04:00", "bedtime_end": "2026-10-20T23:30:00-04:00",
        "latency": 240,
        "sleep_phase_30_sec": codes,
        "sleep_phase_5_min": "421234",
        "movement_30_sec": format!("{}{}{}", "1".repeat(40), "2".repeat(15), "3".repeat(5)),
    })
}

#[test]
fn prefers_the_30_second_series_and_decodes_codes() {
    let h = Hypnogram::from_doc(&night()).unwrap();
    assert_eq!(h.epoch_secs, 30);
    assert_eq!(h.epochs.len(), 60);
    assert_eq!(h.epochs[0], Some(Stage::Awake));
    assert_eq!(h.epochs[8], Some(Stage::Light));
    assert_eq!(h.epochs[18], Some(Stage::Deep));
    let five = Hypnogram::from_doc(&json!({"bedtime_start": "2026-10-20T23:00:00-04:00", "sleep_phase_5_min": "4x12"})).unwrap();
    assert_eq!(five.epoch_secs, 300);
    assert_eq!(five.epochs, vec![Some(Stage::Awake), None, Some(Stage::Deep), Some(Stage::Light)], "unknown codes are gaps");
    let app = Hypnogram::from_doc(&json!({"bedtime_start": "2026-10-20T23:00:00-04:00", "app_sleep_phase_5_min": "3"})).unwrap();
    assert_eq!(app.epochs, vec![Some(Stage::Rem)]);
    assert!(Hypnogram::from_doc(&json!({"sleep_phase_30_sec": "1"})).is_none(), "needs a start time");
}

#[test]
fn totals_longest_and_stretches() {
    let h = Hypnogram::from_doc(&night()).unwrap();
    let stats: std::collections::BTreeMap<Stage, _> = stage_stats(&h).into_iter().collect();
    assert_eq!(stats[&Stage::Deep].seconds, 360);
    assert_eq!(stats[&Stage::Light].seconds, 480);
    assert_eq!(stats[&Stage::Light].longest_seconds, 300);
    assert_eq!(stats[&Stage::Light].stretches_5min, 1, "the 3-minute light stretch is under 5 minutes");
    assert_eq!(stats[&Stage::Rem].seconds, 300);
    assert_eq!(stats[&Stage::Awake].seconds, 660);
    assert_eq!(stats[&Stage::Awake].stretches_5min, 1);
    let total: i64 = stats.values().map(|s| s.seconds).sum();
    assert_eq!(total, 1800);
}

#[test]
fn onset_uses_oura_latency_and_counts_wake_between() {
    let h = Hypnogram::from_doc(&night()).unwrap();
    let e = events(&h, Some(240.0));
    let t = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap();
    assert_eq!(e.fell_asleep, Some(t("2026-10-20T23:04:00-04:00")));
    assert_eq!(e.first_sleep_epoch, Some(t("2026-10-20T23:04:00-04:00")));
    assert_eq!(e.first_deep, Some(t("2026-10-20T23:09:00-04:00")));
    assert_eq!(e.first_rem, Some(t("2026-10-20T23:19:00-04:00")));
    assert_eq!(e.woke_for_good, Some(t("2026-10-20T23:24:00-04:00")));
    assert_eq!(e.awake_between_seconds, 60, "the 1-minute wake inside the sleep window");
    assert_eq!(e.awakenings_5min, 0);
    // Oura's onset can be later than the first sleep epoch.
    let late = events(&h, Some(600.0));
    assert_eq!(late.fell_asleep, Some(t("2026-10-20T23:10:00-04:00")));
    assert_eq!(late.first_sleep_epoch, Some(t("2026-10-20T23:04:00-04:00")));
    assert_eq!(events(&h, None).fell_asleep, Some(t("2026-10-20T23:04:00-04:00")), "no latency: first sleep epoch");
}

#[test]
fn movement_shares() {
    let m = movement(&night()).unwrap();
    assert_eq!(m, vec![("still", 1200), ("restless", 450), ("tossing and turning", 150), ("active", 0)]);
    assert!(movement(&json!({})).is_none());
}

#[test]
fn chart_has_four_rows_fits_width_and_marks_hours() {
    let h = Hypnogram::from_doc(&night()).unwrap();
    let chart = render_chart(&h, 20);
    let lines: Vec<&str> = chart.lines().collect();
    assert!(lines[0].starts_with("  awake") && lines[1].starts_with("  REM") && lines[2].starts_with("  light") && lines[3].starts_with("  deep"));
    for row in &lines[..4] {
        assert!(row.chars().count() <= 8 + 20, "{row}");
    }
    assert!(lines[4].trim_start().starts_with("23:00"), "{chart}");
    assert!(lines[5].contains("one column ≈ 2m"), "60 epochs in 20 columns: 3 epochs each");
    // Each column has exactly one stage.
    let cols = lines[..4].iter().map(|l| l.chars().skip(8).filter(|c| *c == '█').count()).sum::<usize>();
    assert_eq!(cols, 20);
}

#[test]
fn period_text_and_segments() {
    let out = render_period(&night(), 30, true);
    assert!(out.starts_with("long sleep · 23:00 → 23:30 (-04:00) · 30-second stages\n"), "{out}");
    assert!(out.contains("fell asleep       23:04 (4m after bedtime)"), "{out}");
    assert!(out.contains("first deep        23:09 (5m after falling asleep)"));
    assert!(out.contains("still 67% · restless 25% · tossing and turning 8% · active 0%"));
    assert!(out.contains("23:00:00–23:04:00  awake  4m"));
    assert!(out.contains("23:15:00–23:16:00  awake  1m"));
    // A sub-minute run keeps its seconds.
    let blip = json!({"type": "sleep", "bedtime_start": "2026-10-20T23:00:00-04:00", "sleep_phase_30_sec": "4441444"});
    assert!(render_period(&blip, 30, true).contains("23:01:30–23:02:00  deep   30s"));
    // A period whose timezone changed mid-night keeps the start offset and says so.
    let travel = json!({"type": "sleep", "bedtime_start": "2026-03-28T01:10:00+02:00", "bedtime_end": "2026-03-28T03:20:00+01:00", "sleep_phase_30_sec": "4222"});
    let out = render_period(&travel, 30, false);
    assert!(out.contains("01:10 → 04:20 (+02:00)") && out.contains("it ended at 03:20 local (+01:00)"), "{out}");
    assert!(render_period(&json!({"type": "late_nap"}), 30, false).contains("no stage data"));
}

#[test]
fn json_has_totals_events_and_segments() {
    let v = period_json(&night());
    assert_eq!(v["epoch_seconds"], 30);
    assert_eq!(v["totals"]["deep"]["seconds"], 360);
    assert_eq!(v["totals"]["rem"]["seconds"], 300);
    assert_eq!(v["events"]["awake_between_seconds"], 60);
    assert_eq!(v["movement_seconds"]["restless"], 450);
    let segs = v["segments"].as_array().unwrap();
    assert_eq!(segs.len(), 7);
    assert_eq!(segs[0], json!({"start": "2026-10-20T23:00:00-04:00", "end": "2026-10-20T23:04:00-04:00", "stage": "awake", "seconds": 240}));
    assert_eq!(segs[2]["stage"], "deep");
    assert_eq!(period_json(&json!({"id": "x", "type": "sleep"}))["stages"], Value::Null);
}

/// Found by the robustness property (2026-10-21): a corrupted, absurd latency
/// overflowed chrono and panicked. It is now treated as unknown.
#[test]
fn absurd_latency_is_ignored_not_a_panic() {
    let mut doc = night();
    for latency in [json!(776603377078034416i64), json!(1e300), json!(-5), json!(100_000)] {
        doc["latency"] = latency;
        let h = Hypnogram::from_doc(&doc).unwrap();
        let e = events(&h, doc["latency"].as_f64());
        assert_eq!(e.fell_asleep, e.first_sleep_epoch, "an onset outside the period falls back to the first sleep");
        let _ = render_period(&doc, 30, true);
        let _ = period_json(&doc);
    }
}
