//! Property-based tests (Hegel) for somno's pure core.
//!
//! Each test name is a sentence about how somno behaves, and the comment
//! above it says why that matters when you read your sleep data. Hegel tries
//! thousands of inputs per test (see `hegel.toml`); when one breaks the
//! sentence it shrinks it to the smallest example and prints it.
//!
//! Most properties compare somno against a model built by the generator
//! (`tests/common`): a night as runs of stages, periods across days, dated
//! documents. Others change the input in a way whose effect is known
//! (shuffle it, split a range, rewrite timestamps in another UTC offset).
//!
//! `cargo test --test properties` runs them; `HEGEL_TEST_CASES=20000` digs
//! deeper and `HEGEL_STATISTICS=1 ... -- --nocapture` shows what was generated.
//!
//! Not covered here, because it lives behind the network or the terminal:
//! OAuth login and token refresh, Eight Sleep login, HTTP requests, and the
//! command-line wiring in `src/main.rs`. `tests/store.rs` covers files on disk.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, Offset, Timelike};
use chrono_tz::Tz;
use common::*;
use hegel::prelude::*;
use serde_json::{json, Value};
use somno::night::{self, Night, Slot, Source};
use somno::stages::{events, movement, period_json, render_chart, stage_stats, Hypnogram, Stage};
use somno::types::{days_inclusive, fmt_duration, parse_timestamp, DaySpec};
use somno::{eight, http, log, oura, views};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn ny() -> Tz {
    "America/New_York".parse().unwrap()
}

fn ts(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

fn d(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn zone(name: &str) -> Tz {
    name.parse().unwrap()
}

/// Display zones with unusual clocks: US Eastern; Lord Howe (30-minute DST);
/// Kathmandu (+05:45); Havana, Santiago and Beirut (DST skips midnight).
const ZONES: [&str; 6] = ["America/New_York", "Australia/Lord_Howe", "Asia/Kathmandu", "America/Havana", "America/Santiago", "Asia/Beirut"];

/// The documents in an order the test draws.
fn shuffle(tc: &TestCase, docs: &[Value]) -> Vec<Value> {
    let order: Vec<usize> = tc.draw(gs::vecs(gs::integers::<usize>()).min_size(docs.len()).max_size(docs.len()));
    let mut keyed: Vec<(usize, Value)> = order.into_iter().zip(docs.iter().cloned()).collect();
    keyed.sort_by_key(|(k, _)| *k);
    keyed.into_iter().map(|(_, doc)| doc).collect()
}

/// Adjacent runs of the same stage merged: the structure the decoder must find.
fn merged(runs: &[(char, usize)]) -> Vec<(char, usize)> {
    let mut out: Vec<(char, usize)> = Vec::new();
    for &(c, n) in runs {
        match out.last_mut() {
            Some((last, len)) if *last == c => *len += n,
            _ => out.push((c, n)),
        }
    }
    out
}

fn stage_code(s: Stage) -> char {
    match s {
        Stage::Deep => '1',
        Stage::Light => '2',
        Stage::Rem => '3',
        Stage::Awake => '4',
    }
}

/// The value after a row's label in rendered text.
fn field<'a>(out: &'a str, label: &str) -> &'a str {
    out.lines().find_map(|l| l.strip_prefix(label)).unwrap_or_else(|| panic!("no {label} row in\n{out}")).trim()
}

/// The latency somno should use, in whole seconds: within the period and no
/// later than the end of the last sleep epoch, else none.
fn usable_latency(p: &Period) -> Option<i64> {
    let last_sleep_end = p.codes().rfind(|c| c != '4').map(|b| (b as i64 + 1) * 30);
    p.latency
        .filter(|l| l.is_finite() && *l >= 0.0 && *l <= (p.epochs() * 30) as f64)
        .map(|l| l as i64)
        .filter(|l| last_sleep_end.is_none_or(|end| *l <= end))
}

const COLLECTIONS: [&str; 3] = ["sleep", "rest_mode_period", "enhanced_tag"];

// ---------------------------------------------------------------------------
// Days you type: today, yesterday, -N, YYYY-MM-DD
// ---------------------------------------------------------------------------

/// Something shaped like a day: a keyword, `-N` for any integer (so `--N`),
/// `+N`, a date, a date at the edge of the calendar or with a signed or
/// six-digit year, or arbitrary text.
#[hegel::composite]
fn typed_days(tc: &TestCase) -> String {
    tc.draw(hegel::one_of!(
        gs::sampled_from(&["today", "yesterday", "tomorrow", " Yesterday ", "TODAY", ""][..]).map(|w: &str| w.to_string()),
        gs::integers::<i64>().map(|n| format!("-{n}")),
        gs::integers::<i64>().map(|n| format!("+{n}")),
        any_day().map(|day| day.format("%Y-%m-%d").to_string()),
        gs::sampled_from(&["+262142-12-31", "-262143-01-01", "262142-12-31", "9999-12-31", "0000-01-01"][..]).map(|w: &str| w.to_string()),
        gs::from_regex(r"[+-]?[0-9]{4,6}-[0-9]{2}-[0-9]{2}"),
        gs::text(),
    ))
}

/// Any day somno accepts can be looked up (a window around it, a week
/// ending on it) without crashing, however far from today it is.
#[hegel::test]
fn any_accepted_day_can_be_looked_up_without_crashing(tc: TestCase) {
    let typed = tc.draw(typed_days());
    let today = tc.draw(any_day());
    let Ok(spec) = typed.parse::<DaySpec>() else { return };
    let day = spec.resolve(today);
    let (a, b) = oura::query_window("enhanced_tag", day, day);
    let _ = oura::window_params("heartrate", Some(a), Some(b + Duration::days(1)), ny());
    assert_eq!(days_inclusive(day - Duration::days(6), day).len(), 7);
}

/// Typing anything as a day gives a day or an error message, never a crash.
#[hegel::test]
fn any_typed_day_is_accepted_or_rejected_without_crashing(tc: TestCase) {
    let typed = tc.draw(typed_days());
    if let Ok(spec) = typed.parse::<DaySpec>() {
        let _ = spec.resolve(date(2026, 10, 21));
    }
}

/// A date you type is exactly the day somno looks up.
#[hegel::test]
fn a_typed_date_is_that_date(tc: TestCase) {
    let day = tc.draw(hegel::extras::chrono::naive_dates().min_value(date(0, 1, 1)).max_value(date(9999, 12, 31)));
    let today = tc.draw(any_day());
    let spec: DaySpec = day.format("%Y-%m-%d").to_string().parse().unwrap();
    assert_eq!(spec.resolve(today), day);
}

/// `-N` means N days before today, for any N up to 100000 (`-0` is today).
#[hegel::test]
#[hegel::explicit_test_case(n = 0i64, today = date(2026, 10, 21))]
fn minus_n_means_n_days_before_today(tc: TestCase) {
    let n = tc.draw(gs::integers::<i64>().min_value(0).max_value(100_000));
    let today = tc.draw(any_day());
    let spec: DaySpec = format!("-{n}").parse().unwrap();
    assert_eq!((today - spec.resolve(today)).num_days(), n);
}

/// A day as somno prints it reads back as the same day, so `--json` output and
/// error messages can be pasted back into a command.
#[hegel::test]
fn a_shown_day_reads_back_as_the_same_day(tc: TestCase) {
    let typed = tc.draw(typed_days());
    let today = tc.draw(any_day());
    let Ok(spec) = typed.parse::<DaySpec>() else { return };
    let again: DaySpec = spec.to_string().parse().unwrap_or_else(|e| panic!("{spec} printed from {typed:?} does not parse: {e}"));
    assert_eq!(again.resolve(today), spec.resolve(today));
}

/// "today" and "yesterday" work in any capitalization and with spaces around.
#[hegel::test]
fn day_keywords_ignore_case_and_spacing(tc: TestCase) {
    let word = tc.draw(gs::sampled_from(&["today", "yesterday"][..]));
    let caps: Vec<bool> = tc.draw(gs::vecs(gs::booleans()).min_size(word.len()).max_size(word.len()));
    let spaced = format!("{}{}{}", tc.draw(gs::from_regex(r" *")), word.chars().zip(&caps).map(|(c, up)| if *up { c.to_ascii_uppercase() } else { c }).collect::<String>(), tc.draw(gs::from_regex(r" *")));
    let today = tc.draw(any_day());
    let back = if word == "yesterday" { 1 } else { 0 };
    assert_eq!(spaced.parse::<DaySpec>().unwrap().resolve(today), today - Duration::days(back));
}

/// A malformed offset (`--1`, `-+1`, `+-1`, `- 1`) or one further back than
/// 100000 days is rejected, not misread as a different day.
#[hegel::test]
fn malformed_or_huge_offsets_are_rejected(tc: TestCase) {
    let typed = tc.draw(hegel::one_of!(
        gs::from_regex(r"(--|-\+|\+-|- )[0-9]{1,25}"),
        gs::integers::<u64>().min_value(100_001).map(|n| format!("-{n}")),
        gs::from_regex(r"-[1-9][0-9]{19,39}"),
    ));
    assert!(typed.parse::<DaySpec>().is_err(), "{typed:?} was accepted");
}

/// Every day from the first to the last of a range is listed once, in order.
#[hegel::test]
fn a_range_lists_every_day_once_in_order(tc: TestCase) {
    let start = tc.draw(any_day());
    let len = tc.draw(gs::integers::<i64>().min_value(0).max_value(800));
    let end = start + Duration::days(len);
    let days = days_inclusive(start, end);
    assert_eq!((days.first(), days.last(), days.len() as i64), (Some(&start), Some(&end), len + 1));
    assert!(days.windows(2).all(|w| (w[1] - w[0]).num_days() == 1));
}

/// A range whose end is before its start lists no days.
#[hegel::test]
fn a_reversed_range_lists_no_days(tc: TestCase) {
    let end = tc.draw(any_day());
    let back = tc.draw(gs::integers::<i64>().min_value(1).max_value(800));
    assert!(days_inclusive(end + Duration::days(back), end).is_empty());
}

// ---------------------------------------------------------------------------
// Durations and timestamps
// ---------------------------------------------------------------------------

/// Read "6h 05m" / "39m" back into minutes.
fn read_minutes(s: &str) -> i64 {
    match s.split_once("h ") {
        Some((h, m)) => {
            let m: i64 = m.trim_end_matches('m').parse().unwrap();
            assert!(m < 60, "{s} has 60+ minutes with hours");
            h.parse::<i64>().unwrap() * 60 + m
        }
        None => s.trim_end_matches('m').parse().unwrap(),
    }
}

/// A shown duration is within half a minute of the real one.
#[hegel::test]
fn shown_durations_are_within_half_a_minute(tc: TestCase) {
    let secs = tc.draw(hegel::one_of!(gs::floats::<f64>().min_value(0.0).max_value(1e9), gs::sampled_from(&[0.0, 29.999, 30.0, 3570.0, 3600.0][..])));
    let shown = fmt_duration(secs).unwrap();
    assert!((read_minutes(&shown) as f64 * 60.0 - secs).abs() <= 30.0, "{secs}s shown as {shown}");
}

/// A longer duration never shows as shorter.
#[hegel::test]
fn longer_durations_never_show_shorter(tc: TestCase) {
    let a = tc.draw(gs::floats::<f64>().min_value(0.0).max_value(1e9));
    let b = tc.draw(gs::floats::<f64>().min_value(a).max_value(1e9));
    assert!(read_minutes(&fmt_duration(a).unwrap()) <= read_minutes(&fmt_duration(b).unwrap()));
}

/// A negative, infinite, NaN or impossibly long duration is unknown (shown as —),
/// never a made-up number.
#[hegel::test]
fn impossible_durations_are_unknown(tc: TestCase) {
    let secs = tc.draw(hegel::one_of!(
        gs::floats::<f64>().max_value_exclusive(0.0),
        gs::floats::<f64>().min_value_exclusive(1e9),
        gs::sampled_from(&[f64::NAN, f64::INFINITY, f64::NEG_INFINITY][..]),
    ));
    assert_eq!(fmt_duration(secs), None, "{secs}");
}

/// A timestamp reads back as the same instant and keeps its own UTC offset,
/// which is how bedtimes abroad stay in local time.
#[hegel::test]
fn timestamps_keep_their_instant_and_offset(tc: TestCase) {
    let day = tc.draw(any_day());
    let secs = tc.draw(gs::integers::<i64>().min_value(0).max_value(86_399));
    let millis = tc.draw(gs::integers::<i64>().min_value(0).max_value(999));
    let offset = tc.draw(offsets());
    let t = (chrono::Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0).unwrap()) + Duration::seconds(secs) + Duration::milliseconds(millis)).with_timezone(&offset);
    let back = parse_timestamp(&t.to_rfc3339()).unwrap();
    assert_eq!((back, *back.offset()), (t, offset));
}

use chrono::TimeZone as _;

// ---------------------------------------------------------------------------
// Stages: what happened inside a night
// ---------------------------------------------------------------------------

/// Decoding a night's stage codes recovers its stretches of each stage, in order.
#[hegel::test]
fn decoding_recovers_the_stages_in_order(tc: TestCase) {
    let p = tc.draw(period());
    let Some(h) = Hypnogram::from_doc(&p.doc()) else {
        assert!(p.runs.is_empty(), "only an empty stage string means no stage data");
        return;
    };
    let decoded: Vec<(char, usize)> = h.runs().iter().map(|r| (stage_code(r.stage.unwrap()), r.len)).collect();
    assert_eq!(decoded, merged(&p.runs));
}

/// Time in each stage is the count of its 30-second codes, so it matches Oura's
/// own deep, light and REM durations.
#[hegel::test]
fn stage_totals_are_their_30_second_counts(tc: TestCase) {
    let p = tc.draw(period());
    let Some(h) = Hypnogram::from_doc(&p.doc()) else { return };
    let stats = stage_stats(&h);
    assert_eq!(stats.iter().map(|(s, _)| *s).collect::<Vec<_>>(), Stage::ALL.to_vec());
    for (stage, st) in stats {
        assert_eq!(st.seconds, p.secs(stage_code(stage)), "{stage:?}");
    }
}

/// The longest stretch and the count of stretches of 5 minutes or more
/// describe the actual uninterrupted runs.
#[hegel::test]
fn longest_and_5_minute_stretches_describe_the_runs(tc: TestCase) {
    let p = tc.draw(period());
    let Some(h) = Hypnogram::from_doc(&p.doc()) else { return };
    let model = merged(&p.runs);
    for (stage, st) in stage_stats(&h) {
        let mine: Vec<usize> = model.iter().filter(|(c, _)| *c == stage_code(stage)).map(|(_, n)| *n).collect();
        assert_eq!(st.longest_seconds, mine.iter().max().copied().unwrap_or(0) as i64 * 30, "{stage:?}");
        assert_eq!(st.stretches_5min, mine.iter().filter(|n| **n >= 10).count(), "{stage:?}");
    }
}

/// "Fell asleep" is Oura's onset (bedtime + latency) when that lies inside the
/// night; a negative, absurd or missing latency falls back to the first
/// sleep, and never crashes.
#[hegel::test]
fn fell_asleep_is_oura_onset_when_it_lies_inside_the_night(tc: TestCase) {
    let p = tc.draw(period());
    let Some(h) = Hypnogram::from_doc(&p.doc()) else { return };
    let codes: Vec<char> = p.codes().chars().collect();
    let first = codes.iter().position(|c| *c != '4').map(|i| p.start + Duration::seconds(i as i64 * 30));
    let onset = usable_latency(&p).map(|l| p.start + Duration::seconds(l));
    if p.latency.is_some() && onset.is_none() {
        tc.event("latency outside the night or after the last sleep");
    }
    assert_eq!(events(&h, p.latency).fell_asleep, onset.or(first));
}

/// Wake-ups are counted from the "fell asleep" time shown to the end of the
/// last sleep, to the second, so falling asleep isn't counted as waking up.
#[hegel::test]
fn wake_time_counts_from_the_shown_onset_to_the_last_sleep(tc: TestCase) {
    let p = tc.draw(period());
    let Some(h) = Hypnogram::from_doc(&p.doc()) else { return };
    let codes: Vec<char> = p.codes().chars().collect();
    let (Some(a), Some(b)) = (codes.iter().position(|c| *c != '4'), codes.iter().rposition(|c| *c != '4')) else {
        let e = events(&h, p.latency);
        assert_eq!((e.awake_between_seconds, e.awakenings_5min), (0, 0));
        return;
    };
    let (from, to) = (usable_latency(&p).unwrap_or(a as i64 * 30), (b as i64 + 1) * 30);
    let mut stretches: Vec<i64> = Vec::new();
    let mut pos = 0i64;
    for (c, n) in merged(&p.runs) {
        let (lo, hi) = ((pos * 30).max(from), ((pos + n as i64) * 30).min(to));
        if c == '4' && hi > lo {
            stretches.push(hi - lo);
        }
        pos += n as i64;
    }
    let e = events(&h, p.latency);
    assert_eq!(e.awake_between_seconds, stretches.iter().sum::<i64>());
    assert_eq!(e.awakenings_5min, stretches.iter().filter(|s| **s >= 300).count());
}

/// "Fell asleep" is never after "woke for good", and the awake time between
/// them fits between them.
#[hegel::test]
fn falling_asleep_comes_before_waking_for_good(tc: TestCase) {
    let mut p = tc.draw(period());
    // Often an awake tail, and a latency anywhere in the period, so Oura's
    // onset can land after the last sleep.
    if tc.draw(gs::booleans()) {
        p.runs.push(('4', tc.draw(gs::integers::<usize>().min_value(1).max_value(120))));
    }
    if tc.draw(gs::booleans()) {
        p.latency = Some(tc.draw(gs::integers::<i64>().min_value(0).max_value(p.epochs() as i64 * 30)) as f64);
    }
    let Some(h) = Hypnogram::from_doc(&p.doc()) else { return };
    let e = events(&h, p.latency);
    let (Some(fell), Some(woke)) = (e.fell_asleep, e.woke_for_good) else { return };
    assert!(fell <= woke, "fell asleep {fell} after woke for good {woke}");
    assert!(e.awake_between_seconds <= (woke - fell).num_seconds());
}

/// First deep sleep, first REM, first sleep and the end of the last sleep are
/// the times of those epochs.
#[hegel::test]
fn first_deep_rem_and_last_sleep_are_their_epochs(tc: TestCase) {
    let p = tc.draw(period());
    let Some(h) = Hypnogram::from_doc(&p.doc()) else { return };
    let codes: Vec<char> = p.codes().chars().collect();
    let at = |i: usize| p.start + Duration::seconds(i as i64 * 30);
    let e = events(&h, p.latency);
    assert_eq!(e.first_deep, codes.iter().position(|c| *c == '1').map(at));
    assert_eq!(e.first_rem, codes.iter().position(|c| *c == '3').map(at));
    assert_eq!(e.first_sleep_epoch, codes.iter().position(|c| *c != '4').map(at));
    assert_eq!(e.woke_for_good, codes.iter().rposition(|c| *c != '4').map(|i| at(i + 1)));
}

/// Movement shares count each 30 seconds: still, restless, tossing, active.
/// An empty movement string means no movement data, not zero movement.
#[hegel::test]
fn movement_counts_each_30_seconds(tc: TestCase) {
    let p = tc.draw(period());
    let count = |code: char| p.movement.iter().filter(|c| **c == code).count() as i64 * 30;
    let expected = (!p.movement.is_empty()).then(|| vec![("still", count('1')), ("restless", count('2')), ("tossing and turning", count('3')), ("active", count('4'))]);
    assert_eq!(movement(&p.doc()), expected);
}

/// Each segment's length in the text is exact to the second.
#[hegel::test]
fn segment_lengths_in_the_text_are_exact(tc: TestCase) {
    let p = tc.draw(period());
    let width = tc.draw(gs::integers::<usize>().min_value(20).max_value(400));
    let out = somno::stages::render_period(&p.doc(), width, true);
    let Some((_, segments)) = out.split_once("\n  segments\n") else { return };
    let seconds = |token: &str| -> i64 {
        let (n, unit) = token.split_at(token.len() - 1);
        n.parse::<i64>().unwrap() * match unit { "h" => 3600, "m" => 60, "s" => 1, u => panic!("unit {u} in {token}") }
    };
    let shown: Vec<i64> = segments.lines().map(|l| l.split_whitespace().skip(2).map(seconds).sum()).collect();
    let model: Vec<i64> = merged(&p.runs).iter().map(|(_, n)| *n as i64 * 30).collect();
    assert_eq!(shown, model);
}

/// The JSON segments cover the night exactly: back to back, right stage,
/// right length, ending when the time in bed ends.
#[hegel::test]
fn segments_cover_the_night_back_to_back(tc: TestCase) {
    let p = tc.draw(period());
    let v = period_json(&p.doc());
    let Some(segs) = v["segments"].as_array() else {
        assert!(p.runs.is_empty());
        return;
    };
    let model = merged(&p.runs);
    assert_eq!(segs.len(), model.len());
    let mut cursor = p.start;
    for (s, (code, n)) in segs.iter().zip(&model) {
        let (start, end) = (ts(s["start"].as_str().unwrap()), ts(s["end"].as_str().unwrap()));
        assert_eq!(start, cursor);
        assert_eq!(((end - start).num_seconds(), s["seconds"].as_i64()), (*n as i64 * 30, Some(*n as i64 * 30)));
        assert_eq!(s["stage"].as_str(), Some(["deep", "light", "rem", "awake"][(*code as u8 - b'1') as usize]));
        cursor = end;
    }
    assert_eq!((cursor - p.start).num_seconds(), p.epochs() as i64 * 30);
}

/// Each chart column shows the stage you were in most during its slice of the
/// night, or nothing for a slice of unknown codes, and the chart fits the width.
#[hegel::test]
fn chart_columns_show_their_most_common_stage(tc: TestCase) {
    let codes = tc.draw(gs::from_regex(r"[1-4]{0,800}|[1-4x?]{1,800}"));
    let width = tc.draw(gs::integers::<usize>().min_value(20).max_value(200));
    let Some(h) = Hypnogram::from_doc(&json!({"bedtime_start": "2026-10-18T00:00:00Z", "sleep_phase_30_sec": codes})) else { return };
    let chart = render_chart(&h, width);
    let rows: Vec<Vec<char>> = chart.lines().take(4).map(|l| l.chars().skip(8).collect()).collect();
    let per = h.epochs.len().div_ceil(width);
    assert!(rows.iter().all(|r| r.len() <= width));
    for (col, bin) in h.epochs.chunks(per).enumerate() {
        let marked: Vec<Stage> = [Stage::Awake, Stage::Rem, Stage::Light, Stage::Deep].into_iter().zip(&rows).filter(|(_, r)| r.get(col) == Some(&'█')).map(|(s, _)| s).collect();
        let count = |s: Stage| bin.iter().filter(|x| **x == Some(s)).count();
        let best = Stage::ALL.into_iter().map(count).max().unwrap();
        if best == 0 {
            assert!(marked.is_empty(), "column {col} of unknown codes marks {marked:?}");
        } else {
            assert_eq!(marked.len(), 1, "column {col} marks {marked:?}");
            assert_eq!(count(marked[0]), best, "column {col} is not its slice's most common stage");
        }
    }
}

/// The chart's hour marks name the hour that starts at their column, every
/// hour that has room gets one, and the scale line is right.
#[hegel::test]
fn chart_hour_marks_are_complete_and_correct(tc: TestCase) {
    let p = tc.draw(period());
    let width = tc.draw(gs::integers::<usize>().min_value(20).max_value(200));
    let Some(h) = Hypnogram::from_doc(&p.doc()) else { return };
    let chart = render_chart(&h, width);
    let lines: Vec<&str> = chart.lines().collect();
    let per = h.epochs.len().div_ceil(width);
    let columns = h.epochs.len().div_ceil(per);
    let axis: Vec<char> = lines[4].chars().skip(8).collect();
    let mut labels: Vec<(usize, String)> = Vec::new();
    let mut i = 0;
    while i < axis.len() {
        if axis[i] == ' ' {
            i += 1;
            continue;
        }
        let start = i;
        while i < axis.len() && axis[i] != ' ' {
            i += 1;
        }
        labels.push((start, axis[start..i].iter().collect()));
    }
    let mut want = vec![(0, h.start.format("%H:%M").to_string())];
    let mut room_from = 6;
    for col in 1..columns {
        if h.at((col - 1) * per).hour() != h.at(col * per).hour() && col >= room_from && col + 5 <= columns + 6 {
            want.push((col, h.at(col * per).format("%H:00").to_string()));
            room_from = col + 6;
        }
    }
    assert_eq!(labels, want);
    assert_eq!(lines[5].trim(), format!("one column ≈ {}", fmt_duration((per as i64 * h.epoch_secs) as f64).unwrap()));
}

/// The same night sent as 5-minute codes gives the same totals, onset,
/// wake-ups and segments as its 30-second codes, whatever Oura's latency.
#[hegel::test]
fn five_minute_and_30_second_stages_agree(tc: TestCase) {
    let codes = tc.draw(gs::from_regex(r"[1-4x]{1,80}"));
    // Often a latency that isn't a whole number of 5-minute epochs.
    let latency = tc.draw(hegel::one_of!(
        gs::just(None),
        gs::integers::<u32>().max_value(80 * 300).map(|s| Some(f64::from(s))),
        finite().map(Some),
    ));
    let coarse = json!({"bedtime_start": "2026-10-18T00:00:00Z", "sleep_phase_5_min": codes});
    let fine = json!({"bedtime_start": "2026-10-18T00:00:00Z", "sleep_phase_30_sec": codes.chars().flat_map(|c| std::iter::repeat_n(c, 10)).collect::<String>()});
    let (a, b) = (Hypnogram::from_doc(&coarse).unwrap(), Hypnogram::from_doc(&fine).unwrap());
    assert_eq!(stage_stats(&a), stage_stats(&b));
    assert_eq!(events(&a, latency), events(&b, latency));
    assert_eq!(period_json(&coarse)["segments"], period_json(&fine)["segments"]);
}

/// The 30-second codes win when present; a missing, empty or "None" 30-second
/// string falls back to the 5-minute codes, then to the app's.
#[hegel::test]
fn the_finest_stage_series_wins(tc: TestCase) {
    let five = tc.draw(gs::from_regex(r"[1-4]{1,40}"));
    let thirty = tc.draw(gs::from_regex(r"[1-4]{1,40}"));
    let absent = tc.draw(gs::sampled_from(&[Value::Null, json!(""), json!("None")][..]));
    let start = "2026-10-18T00:00:00Z";
    let both = json!({"bedtime_start": start, "sleep_phase_30_sec": thirty, "sleep_phase_5_min": five, "app_sleep_phase_5_min": "3"});
    assert_eq!(Hypnogram::from_doc(&both).unwrap().epoch_secs, 30);
    let fallback = json!({"bedtime_start": start, "sleep_phase_30_sec": absent, "sleep_phase_5_min": five});
    assert_eq!(Hypnogram::from_doc(&fallback).unwrap().epochs.len(), five.len());
    let app = json!({"bedtime_start": start, "app_sleep_phase_5_min": five});
    assert_eq!(Hypnogram::from_doc(&app).unwrap().epochs.len(), five.len());
}

/// An unknown code keeps its 30 seconds as a gap: it splits stretches, counts
/// in no stage, and doesn't shift anything after it.
#[hegel::test]
fn unknown_codes_keep_their_time(tc: TestCase) {
    let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(40));
    let gap = tc.draw(gs::integers::<usize>().min_value(1).max_value(40));
    let unknown = tc.draw(gs::characters().exclude_characters("1234"));
    let doc = json!({"bedtime_start": "2026-10-18T00:00:00Z", "sleep_phase_30_sec": format!("{}{}{}", "1".repeat(n), unknown.to_string().repeat(gap), "1".repeat(n))});
    let h = Hypnogram::from_doc(&doc).unwrap();
    let deep = &stage_stats(&h)[0].1;
    assert_eq!((deep.seconds, deep.longest_seconds), (60 * n as i64, 30 * n as i64));
    let segs = period_json(&doc)["segments"].as_array().unwrap().clone();
    assert!(segs[1]["stage"].is_null());
    assert_eq!(ts(segs[2]["start"].as_str().unwrap()), h.start + Duration::seconds(30 * (n + gap) as i64));
}

/// Writing the same bedtime in another UTC offset changes no totals or times.
#[hegel::test]
fn stages_ignore_how_the_bedtime_is_written(tc: TestCase) {
    let p = tc.draw(period());
    let other = tc.draw(offsets());
    let mut moved = p.clone();
    moved.start = p.start.with_timezone(&other);
    let (Some(h1), Some(h2)) = (Hypnogram::from_doc(&p.doc()), Hypnogram::from_doc(&moved.doc())) else { return };
    assert_eq!(stage_stats(&h1), stage_stats(&h2));
    assert_eq!(events(&h1, p.latency), events(&h2, p.latency));
}

// ---------------------------------------------------------------------------
// Oura: which period is your night
// ---------------------------------------------------------------------------

/// The night shown for a day is its longest confirmed long sleep (else its
/// longest confirmed period), with that period's own numbers, and other
/// confirmed periods counted as naps.
#[hegel::test]
fn the_night_is_the_longest_confirmed_long_sleep(tc: TestCase) {
    let ps = tc.draw(periods());
    let docs: Vec<Value> = ps.iter().map(Period::doc).collect();
    let days: Vec<NaiveDate> = ps.iter().map(|p| p.day).collect::<BTreeSet<_>>().into_iter().collect();
    let nights = oura::summarize(&docs, &[], &[], &days);
    for day in &days {
        let live: Vec<&Period> = ps.iter().filter(|p| p.day == *day && p.confirmed()).collect();
        let longs: Vec<&Period> = live.iter().copied().filter(|p| p.kind == "long_sleep").collect();
        let pool = if longs.is_empty() { &live } else { &longs };
        let Some(best) = pool.iter().map(|p| p.asleep_secs()).max() else {
            assert!(!nights.contains_key(day));
            continue;
        };
        let n = &nights[day];
        // Oura's ids are unique, but if two periods ever share one, the night
        // must still be one of them, with that period's own numbers.
        let candidates: Vec<&&Period> = pool.iter().filter(|p| Some(p.id.as_str()) == n.source_id.as_deref()).collect();
        assert!(
            candidates.iter().any(|c| c.asleep_secs() == best && (n.hrv_avg_ms, n.hr_lowest_bpm) == (c.metrics[0], c.metrics[1])),
            "the night is not the longest period of the right pool with its own numbers"
        );
        assert_eq!(n.other_periods, Some(live.len() - 1));
        assert_eq!(n.asleep_s, Some(best as f64));
    }
}

/// When two periods tie for longest, the earlier bedtime wins (then the
/// smaller id), so the same data always gives the same night.
#[hegel::test]
fn ties_go_to_the_earlier_bedtime(tc: TestCase) {
    let ps = tc.draw(periods());
    let docs: Vec<Value> = ps.iter().map(Period::doc).collect();
    for day in ps.iter().map(|p| p.day).collect::<BTreeSet<_>>() {
        let refs: Vec<&Value> = docs.iter().filter(|doc| doc["day"] == day.to_string()).collect();
        let Some(main) = oura::main_period(&refs) else { continue };
        let live: Vec<&Period> = ps.iter().filter(|p| p.day == day && p.confirmed()).collect();
        let longs: Vec<&Period> = live.iter().copied().filter(|p| p.kind == "long_sleep").collect();
        let pool = if longs.is_empty() { live } else { longs };
        let best = pool.iter().map(|p| p.asleep_secs()).max().unwrap();
        let first = pool.iter().filter(|p| p.asleep_secs() == best).min_by_key(|p| (p.start, p.id.clone())).unwrap();
        assert_eq!(main["id"].as_str(), Some(first.id.as_str()));
    }
}

/// A period with no readable bedtime never wins a tie against one with a
/// bedtime.
#[hegel::test]
fn a_period_without_a_bedtime_never_wins_a_tie(tc: TestCase) {
    let mut p = tc.draw(period());
    p.kind = "long_sleep".into();
    let known = p.doc();
    let mut unknown = p.doc();
    unknown["id"] = json!("zzzzzzzz");
    unknown["bedtime_start"] = tc.draw(gs::sampled_from(&[Value::Null, json!("not a time"), json!(7)][..]));
    let pair = if tc.draw(gs::booleans()) { [&known, &unknown] } else { [&unknown, &known] };
    assert_eq!(oura::main_period(&pair).unwrap()["id"], known["id"]);
}

/// Daily score and readiness documents, sometimes two for the same day with
/// different scores.
#[hegel::composite]
fn daily_docs(tc: &TestCase) -> Vec<Value> {
    let n = tc.draw(gs::integers::<usize>().max_value(8));
    (0..n)
        .map(|_| {
            let day = date(2026, 10, 12) + Duration::days(tc.draw(gs::integers::<i64>().min_value(0).max_value(9)));
            let score = tc.draw(gs::sampled_from(&[60, 75, 90][..]));
            json!({"id": tc.draw(gs::sampled_from(&["a", "b"][..])), "day": day.to_string(), "score": score, "temperature_deviation": score as f64 / 100.0})
        })
        .collect()
}

/// The order Oura returns periods and daily documents in never changes the
/// nights, even when a day has two daily documents.
#[hegel::test]
fn api_order_never_changes_the_nights(tc: TestCase) {
    let ps = tc.draw(periods());
    let docs: Vec<Value> = ps.iter().map(Period::doc).collect();
    let (scores, readiness) = (tc.draw(daily_docs()), tc.draw(daily_docs()));
    let days: Vec<NaiveDate> = ps.iter().map(|p| p.day).chain((0..10).map(|n| date(2026, 10, 12) + Duration::days(n))).collect::<BTreeSet<_>>().into_iter().collect();
    let before = oura::summarize(&docs, &scores, &readiness, &days);
    let (docs, scores, readiness) = (shuffle(&tc, &docs), shuffle(&tc, &scores), shuffle(&tc, &readiness));
    assert_eq!(oura::summarize(&docs, &scores, &readiness, &days), before);
}

/// Deleted periods and rejected "rest" detections never become your night or
/// count as naps, however long they are.
#[hegel::test]
fn deleted_and_rejected_periods_never_count(tc: TestCase) {
    let hours = tc.draw(gs::integers::<u32>().min_value(1).max_value(12));
    let day = d("2026-10-18");
    let mk = |id: &str, kind: &str, h: u32| json!({"id": id, "day": day.to_string(), "type": kind, "total_sleep_duration": h * 3600});
    let docs = vec![mk("main", "long_sleep", hours), mk("deleted", "deleted", hours + 5), mk("rejected", "rest", hours + 6)];
    let n = &oura::summarize(&docs, &[], &[], &[day])[&day];
    assert_eq!((n.source_id.as_deref(), n.other_periods), (Some("main"), Some(0)));
}

/// A period with no total sleep ranks below one with any total, even 0.
#[hegel::test]
fn a_missing_total_ranks_last(tc: TestCase) {
    let total = tc.draw(gs::integers::<u32>().max_value(10));
    let null_first = tc.draw(gs::booleans());
    let a = json!({"id": "with", "type": "long_sleep", "total_sleep_duration": total});
    let b = json!({"id": "without", "type": "long_sleep", "total_sleep_duration": null});
    let pick = if null_first { oura::main_period(&[&b, &a]) } else { oura::main_period(&[&a, &b]) };
    assert_eq!(pick.and_then(|p| p["id"].as_str()), Some("with"));
}

/// Each Oura field lands in its own place in the night (distinct values catch
/// swaps), and a missing field stays unknown instead of becoming 0.
#[hegel::test]
fn each_oura_field_lands_in_its_own_place(tc: TestCase) {
    let x = tc.draw(gs::integers::<u32>().min_value(1).max_value(100_000));
    let day = d("2026-10-18");
    let pairs = [
        ("time_in_bed", "time_in_bed_s"), ("total_sleep_duration", "asleep_s"), ("deep_sleep_duration", "deep_s"),
        ("rem_sleep_duration", "rem_s"), ("light_sleep_duration", "light_s"), ("awake_time", "awake_s"),
        ("latency", "latency_s"), ("efficiency", "efficiency_pct"), ("average_heart_rate", "hr_avg_bpm"),
        ("lowest_heart_rate", "hr_lowest_bpm"), ("average_hrv", "hrv_avg_ms"), ("average_breath", "breath_avg_per_min"),
        ("restless_periods", "restless_periods"),
    ];
    let missing = tc.draw(gs::sampled_from(&pairs[..]));
    let mut p = json!({"day": day.to_string(), "type": "long_sleep", "id": "main"});
    for (i, (input, _)) in pairs.iter().enumerate() {
        p[*input] = json!(x + i as u32);
    }
    p[missing.0] = Value::Null;
    let v = serde_json::to_value(&oura::summarize(&[p], &[], &[], &[day])[&day]).unwrap();
    for (i, (input, output)) in pairs.iter().enumerate() {
        let want = if *input == missing.0 { None } else { Some(f64::from(x + i as u32)) };
        assert_eq!(v[*output].as_f64(), want, "{input} -> {output}");
    }
}

/// A day with scores but no sleep record still shows its sleep score,
/// readiness and temperature deviation.
#[hegel::test]
fn score_only_days_keep_their_scores(tc: TestCase) {
    let (sleep, ready) = (tc.draw(gs::integers::<u32>().max_value(100)), tc.draw(gs::integers::<u32>().max_value(100)));
    let dev = tc.draw(gs::floats::<f64>().min_value(-5.0).max_value(5.0));
    let day = d("2026-10-18");
    let nights = oura::summarize(&[], &[json!({"day": day.to_string(), "score": sleep})], &[json!({"day": day.to_string(), "score": ready, "temperature_deviation": dev})], &[day]);
    let n = &nights[&day];
    assert_eq!((n.sleep_score, n.readiness_score, n.temp_deviation_c, n.asleep_s), (Some(f64::from(sleep)), Some(f64::from(ready)), Some(dev), None));
}

// ---------------------------------------------------------------------------
// Oura: fetching ranges and pages
// ---------------------------------------------------------------------------

/// A range holds exactly the documents that belong to it: a day document when
/// its day is inside; a span when it overlaps; a rest-mode period with no end
/// is still going; a tag with no end is a single moment. Duplicates are kept.
#[hegel::test]
fn a_range_holds_exactly_its_documents(tc: TestCase) {
    let collection = tc.draw(gs::sampled_from(&COLLECTIONS[..]));
    let docs: Vec<Value> = tc.draw(gs::vecs(collection_docs(collection)).max_size(30));
    let start = tc.draw(days());
    let len = tc.draw(gs::integers::<i64>().min_value(-3).max_value(30));
    let end = start + Duration::days(len);
    let mut expected: Vec<String> = docs
        .iter()
        .filter(|doc| {
            let first = views::doc_day(doc).unwrap();
            let last = match (collection, doc.get("end_day").and_then(Value::as_str)) {
                ("sleep", _) => first,
                (_, Some(e)) => d(e),
                ("enhanced_tag", None) => first,
                (_, None) => NaiveDate::MAX,
            };
            start <= end && first <= end && last >= start
        })
        .map(Value::to_string)
        .collect();
    let mut got: Vec<String> = views::select(collection, docs, start, end).iter().map(Value::to_string).collect();
    got.sort();
    expected.sort();
    assert_eq!(got, expected);
}

/// Looking up a range gives the same documents as looking up its two halves.
#[hegel::test]
fn a_range_equals_its_two_halves(tc: TestCase) {
    let collection = tc.draw(gs::sampled_from(&COLLECTIONS[..]));
    let docs: Vec<Value> = tc.draw(gs::vecs(collection_docs(collection)).max_size(30));
    let start = tc.draw(days());
    let len = tc.draw(gs::integers::<i64>().min_value(0).max_value(30));
    let cut = tc.draw(gs::integers::<i64>().min_value(0).max_value(len));
    let (end, mid) = (start + Duration::days(len), start + Duration::days(cut));
    let set = |s: NaiveDate, e: NaiveDate| views::select(collection, docs.clone(), s, e).iter().map(Value::to_string).collect::<BTreeSet<_>>();
    let mut halves = set(start, mid);
    if mid < end {
        halves.extend(set(mid + Duration::days(1), end));
    }
    assert_eq!(set(start, end), halves);
}

/// Whether Oura's date bounds are inclusive or exclusive never changes what a
/// lookup returns: somno asks for a padded window and filters itself.
#[hegel::test]
fn oura_date_bounds_never_change_a_lookup(tc: TestCase) {
    let collection = tc.draw(gs::sampled_from(&COLLECTIONS[..]));
    let docs: Vec<Value> = tc.draw(gs::vecs(collection_docs(collection)).max_size(30));
    let start = tc.draw(days());
    let end = start + Duration::days(tc.draw(gs::integers::<i64>().min_value(0).max_value(30)));
    let (lo_inclusive, hi_inclusive) = (tc.draw(gs::booleans()), tc.draw(gs::booleans()));
    let (from, to) = oura::query_window(collection, start, end);
    // Spans within the lookback window; older ones are a documented limit.
    let docs: Vec<Value> = docs.into_iter().filter(|doc| views::doc_day(doc).unwrap() > from).collect();
    let served: Vec<Value> = docs.iter().filter(|doc| {
        let day = views::doc_day(doc).unwrap();
        (if lo_inclusive { day >= from } else { day > from }) && (if hi_inclusive { day <= to } else { day < to })
    }).cloned().collect();
    let show = |v: Vec<Value>| v.iter().map(Value::to_string).collect::<Vec<_>>();
    assert_eq!(show(views::select(collection, served, start, end)), show(views::select(collection, docs, start, end)));
}

/// The order Oura sends documents in never changes a listing, even when two
/// share a day and start time.
#[hegel::test]
fn api_order_never_changes_a_listing(tc: TestCase) {
    let collection = tc.draw(gs::sampled_from(&COLLECTIONS[..]));
    let docs: Vec<Value> = tc.draw(gs::vecs(collection_docs(collection)).max_size(12));
    let (start, end) = (tc.draw(days()), tc.draw(days()));
    let show = |v: Vec<Value>| v.iter().map(Value::to_string).collect::<Vec<_>>();
    assert_eq!(show(views::select(collection, shuffle(&tc, &docs), start, end)), show(views::select(collection, docs, start, end)));
}

/// A span whose `day` is null or not a date is listed exactly like one
/// without a `day`.
#[hegel::test]
fn a_null_day_is_the_same_as_no_day(tc: TestCase) {
    let collection = tc.draw(gs::sampled_from(&["rest_mode_period", "enhanced_tag"][..]));
    let doc = tc.draw(collection_docs(collection));
    let mut odd = doc.clone();
    odd["day"] = tc.draw(gs::sampled_from(&[Value::Null, json!("someday"), json!(3)][..]));
    let (start, end) = (tc.draw(days()), tc.draw(days()));
    assert_eq!(views::in_range(collection, &odd, start, end), views::in_range(collection, &doc, start, end));
}

/// Documents are listed in the order they happened, by day and then by their
/// own start time to the millisecond, whatever offset each is written in.
#[hegel::test]
fn documents_list_in_the_order_they_happened(tc: TestCase) {
    let ps = tc.draw(gs::vecs(period()).max_size(10));
    let mut docs: Vec<Value> = ps.iter().map(Period::doc).collect();
    views::sort_docs(&mut docs);
    let keys: Vec<(NaiveDate, DateTime<FixedOffset>)> = docs.iter().map(|doc| (views::doc_day(doc).unwrap(), ts(doc["bedtime_start"].as_str().unwrap()))).collect();
    assert!(keys.windows(2).all(|w| w[0] <= w[1]), "{keys:?}");
}

/// Pages are put back together in order, whatever their sizes, including
/// empty pages in the middle, and each page token is passed back exactly.
#[hegel::test]
fn pages_are_put_back_together_in_order(tc: TestCase) {
    let pages: Vec<Vec<u8>> = tc.draw(gs::vecs(gs::vecs(gs::integers::<u8>()).max_size(5)).min_size(1).max_size(6));
    let token = tc.draw(gs::text().min_size(1));
    let tokens: Vec<Option<String>> = (0..pages.len()).map(|i| (i > 0).then(|| format!("{i}{token}"))).collect();
    let mut calls = 0;
    let got = oura::paginate(|t| {
        anyhow::ensure!(calls < pages.len(), "asked for more pages than exist");
        anyhow::ensure!(t == tokens[calls].as_deref(), "token {:?} came back as {t:?}", tokens[calls]);
        let page = json!({"data": pages[calls], "next_token": tokens.get(calls + 1).cloned().flatten()});
        calls += 1;
        Ok(page)
    })
    .unwrap();
    assert_eq!(got, pages.concat().into_iter().map(Value::from).collect::<Vec<_>>());
}

/// An empty or repeated page token stops with an error instead of looping.
#[hegel::test]
fn a_bad_page_token_stops_instead_of_looping(tc: TestCase) {
    let cycle = tc.draw(gs::integers::<usize>().min_value(1).max_value(8));
    let empty_token = tc.draw(gs::booleans());
    let mut calls = 0;
    let result = oura::paginate(|_| {
        calls += 1;
        anyhow::ensure!(calls <= cycle + 2, "test guard: looping");
        let next = if empty_token { String::new() } else { format!("p{}", (calls - 1) % cycle) };
        Ok(json!({"data": [], "next_token": next}))
    });
    assert!(result.is_err());
    assert_eq!(calls, if empty_token { 1 } else { cycle + 1 });
}

/// A failed page request comes back as that error, not partial data.
#[hegel::test]
fn a_failed_page_returns_its_error(tc: TestCase) {
    let fail_at = tc.draw(gs::integers::<usize>().max_value(8));
    let mut calls = 0;
    let result = oura::paginate(|_| {
        anyhow::ensure!(calls != fail_at, "sentinel fetch error");
        calls += 1;
        Ok(json!({"data": [calls], "next_token": calls.to_string()}))
    });
    assert_eq!(result.unwrap_err().to_string(), "sentinel fetch error");
}

/// Heart-rate time series ask Oura for local midnight to local midnight,
/// so a day is 23 or 25 hours long across a DST switch.
#[hegel::test]
fn heart_rate_windows_run_midnight_to_midnight_locally(tc: TestCase) {
    let start = tc.draw(any_day());
    let end = start + Duration::days(tc.draw(gs::integers::<i64>().min_value(1).max_value(40)));
    let collection = tc.draw(gs::sampled_from(&["heartrate", "ring_battery_level"][..]));
    let params: BTreeMap<String, String> = oura::window_params(collection, Some(start), Some(end), ny()).unwrap().into_iter().collect();
    let (a, b) = (ts(&params["start_datetime"]), ts(&params["end_datetime"]));
    assert_eq!((a.with_timezone(&ny()).date_naive(), a.with_timezone(&ny()).hour()), (start, 0));
    assert_eq!((b.with_timezone(&ny()).date_naive(), b.with_timezone(&ny()).hour()), (end, 0));
}

/// A day's heart-rate window starts at its first local instant in every zone,
/// including zones whose clocks skip midnight (Havana, Santiago, Beirut).
#[hegel::test]
fn every_day_has_a_first_instant_in_every_zone(tc: TestCase) {
    let tz: Tz = tc.draw(hegel::one_of!(
        gs::sampled_from(&ZONES[..]).map(zone),
        gs::sampled_from(&chrono_tz::TZ_VARIANTS[..]),
    ));
    let day = tc.draw(hegel::one_of!(
        gs::sampled_from(&[d("2026-03-08"), d("2026-09-06"), d("2026-03-29"), d("2026-10-04"), d("2026-11-01")][..]),
        any_day(),
    ));
    let params: BTreeMap<String, String> = oura::window_params("heartrate", Some(day), Some(day + Duration::days(1)), tz).unwrap().into_iter().collect();
    let first = ts(&params["start_datetime"]);
    assert_eq!(first.with_timezone(&tz).date_naive(), day);
    assert!((first - Duration::seconds(1)).with_timezone(&tz).date_naive() < day, "{first} is not the first instant of {day} in {tz}");
}

/// Daily collections ask Oura for the dates as given; undated ones for nothing.
#[hegel::test]
fn daily_windows_are_the_dates_given(tc: TestCase) {
    let (start, end) = (tc.draw(any_day()), tc.draw(any_day()));
    let collection = tc.draw(gs::sampled_from(&["sleep", "daily_activity", "workout", "personal_info", "ring_configuration"][..]));
    let params = oura::window_params(collection, Some(start), Some(end), ny()).unwrap();
    if matches!(collection, "personal_info" | "ring_configuration") {
        assert!(params.is_empty());
    } else {
        assert_eq!(params, vec![("start_date".to_string(), start.to_string()), ("end_date".to_string(), end.to_string())]);
    }
}

// ---------------------------------------------------------------------------
// Oura login
// ---------------------------------------------------------------------------

/// Any login code Oura sends back reaches somno intact through the local
/// callback, whatever characters it contains.
#[hegel::test]
fn any_login_code_survives_the_callback(tc: TestCase) {
    let code = tc.draw(gs::text().min_size(1));
    let state = tc.draw(gs::text().min_size(1));
    let url = reqwest::Url::parse_with_params("http://localhost:8765/callback", &[("code", code.as_str()), ("state", state.as_str())]).unwrap();
    assert_eq!(oura::parse_callback(&format!("{}?{}", url.path(), url.query().unwrap()), &state), oura::Callback::Code(code));
}

/// Pasting the redirect URL gives the same code: the whole URL in any letter
/// case, without "http://", just the path, or just the query.
#[hegel::test]
fn a_pasted_redirect_url_gives_the_same_code(tc: TestCase) {
    let code = tc.draw(gs::text().min_size(1));
    let state = tc.draw(gs::text().min_size(1));
    let url = reqwest::Url::parse_with_params("http://localhost:8765/callback", &[("code", code.as_str()), ("state", state.as_str())]).unwrap();
    let query = url.query().unwrap();
    let pasted = match tc.draw(gs::integers::<u8>().max_value(4)) {
        0 => url.to_string(),
        1 => url.as_str().trim_start_matches("http://").to_string(),
        2 => format!("{}?{query}", tc.draw(gs::sampled_from(&["HTTP://LOCALHOST:8765/callback", "Http://LocalHost:8765/callback", "LOCALHOST:8765/callback"][..]))),
        3 => format!("/callback?{query}"),
        _ => format!("?{query}"),
    };
    assert_eq!(oura::parse_pasted(&pasted, &state), oura::Callback::Code(code));
}

/// A redirect carrying any other state, even one differing in two places
/// that a broken comparison could cancel out, is never accepted.
#[hegel::test]
fn a_different_state_is_never_accepted(tc: TestCase) {
    let state = tc.draw(gs::from_regex(r"[a-f0-9]{1,64}"));
    let other = tc.draw(hegel::one_of!(
        gs::text(),
        gs::integers::<u8>().min_value(1).max_value(7).map({
            let state = state.clone();
            move |mask| {
                let mut b = state.clone().into_bytes();
                let last = b.len() - 1;
                b[0] ^= mask;
                if last > 0 {
                    b[last] ^= mask;
                }
                String::from_utf8(b).unwrap()
            }
        }),
    ));
    tc.assume(other != state);
    let target = format!("/callback?code=abc&state={}", url::form_urlencoded::byte_serialize(other.as_bytes()).collect::<String>());
    assert!(!matches!(oura::parse_callback(&target, &state), oura::Callback::Code(_)));
    assert!(!matches!(oura::parse_pasted(&format!("http://localhost:8765{target}"), &state), oura::Callback::Code(_)));
}

/// A redirect with an empty code, or none, is never accepted as a login.
#[hegel::test]
fn an_empty_or_missing_code_is_never_accepted(tc: TestCase) {
    let state = tc.draw(gs::text().min_size(1));
    let enc = url::form_urlencoded::byte_serialize(state.as_bytes()).collect::<String>();
    let target = tc.draw(gs::sampled_from(&[format!("/callback?code=&state={enc}"), format!("/callback?state={enc}")][..]));
    assert!(!matches!(oura::parse_callback(&target, &state), oura::Callback::Code(_)));
}

/// Declining on Oura's page (an error with the right state) ends the login as
/// declined, with Oura's reason.
#[hegel::test]
fn declining_on_ouras_page_is_reported_as_declined(tc: TestCase) {
    let state = tc.draw(gs::from_regex(r"[a-f0-9]{1,64}"));
    let error = tc.draw(gs::from_regex(r"[a-z_]{1,40}"));
    assert_eq!(oura::parse_callback(&format!("/callback?error={error}&state={state}"), &state), oura::Callback::Denied(error));
}

/// Anything pasted that isn't the redirect address (another host or port, a
/// bare code with no state) is never accepted.
#[hegel::test]
fn pasting_something_else_is_never_accepted(tc: TestCase) {
    let state = tc.draw(gs::from_regex(r"[a-f0-9]{1,64}"));
    let pasted = tc.draw(hegel::one_of!(
        gs::from_regex(r"https?://(evil\.example|localhost:9999|127\.0\.0\.1:8765)/callback\?code=x&state=").map({
            let state = state.clone();
            move |u: String| format!("{u}{state}")
        }),
        // Another host or port without a scheme, with the right state.
        gs::from_regex(r"(evil\.example|localhost\.evil\.example|localhost:9999|127\.0\.0\.1:8765)(/[a-z]{0,8})?\?a=1&code=x&state=").map({
            let state = state.clone();
            move |u: String| format!("{u}{state}")
        }),
        gs::from_regex(r"[A-Za-z0-9]{1,30}"),
        gs::text(),
    ));
    assert!(!matches!(oura::parse_pasted(&pasted, &state), oura::Callback::Code(_)), "{pasted:?}");
}

/// The login link asks for your exact redirect address, the state somno
/// checks later, and all eleven Oura data scopes.
#[hegel::test]
fn the_login_link_carries_the_redirect_state_and_every_scope(tc: TestCase) {
    let client = tc.draw(gs::text());
    let state = tc.draw(gs::text());
    let url = reqwest::Url::parse(&oura::authorize_url(&client, &state)).unwrap();
    let q: BTreeMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!((q["client_id"].as_str(), q["state"].as_str(), q["redirect_uri"].as_str()), (client.as_str(), state.as_str(), oura::REDIRECT_URI));
    assert_eq!(q["scope"].split(' ').collect::<BTreeSet<_>>(), oura::SCOPES.iter().copied().collect::<BTreeSet<_>>());
}

/// After login, the scopes reported missing are exactly the ones Oura didn't
/// grant, however Oura prefixes them (`extapi:daily`).
#[hegel::test]
fn missing_scopes_are_exactly_the_ungranted_ones(tc: TestCase) {
    let granted: Vec<&str> = tc.draw(gs::vecs(gs::sampled_from(oura::SCOPES)).max_size(11));
    let prefix = tc.draw(gs::optional(gs::from_regex(r"[a-z]{1,7}")));
    let text = granted.iter().map(|s| prefix.as_ref().map(|p| format!("{p}:{s}")).unwrap_or_else(|| s.to_string())).collect::<Vec<_>>().join(" ");
    let expected: Vec<&str> = oura::SCOPES.iter().copied().filter(|s| !granted.contains(s)).collect();
    assert_eq!(oura::missing_scopes(Some(&text)).unwrap(), expected);
}

/// A token refresh that sends no new refresh token keeps the old one, so the
/// login survives; an expiry is rounded up (so it is never already past) and
/// never overflows, however absurd.
#[hegel::test]
fn token_refreshes_keep_the_login_and_expiries_never_overflow(tc: TestCase) {
    let now = tc.draw(gs::integers::<i64>().min_value(0).max_value(4_000_000_000));
    let expires = tc.draw(gs::floats::<f64>().min_value_exclusive(0.0).allow_infinity(false).allow_nan(false));
    let previous = oura::Tokens { access_token: "a0".into(), refresh_token: "r0".into(), expires_at: 0, scope: Some("daily".into()) };
    let t = oura::tokens_from_response(&json!({"access_token": "a1", "expires_in": expires}), now, Some(&previous)).unwrap();
    assert_eq!((t.refresh_token.as_str(), t.scope.as_deref()), ("r0", Some("daily")));
    assert_eq!(t.expires_at, now.saturating_add(expires.min(10.0 * 365.0 * 86400.0).ceil() as i64));
}

/// `oura configure`'s redirect check passes only when Oura's page goes on to
/// sign-in; a 400 means a browser login would fail too.
#[hegel::test]
fn configure_reads_the_authorization_answer(tc: TestCase) {
    let status = tc.draw(gs::integers::<u16>().min_value(100).max_value(599));
    let verdict = oura::interpret_authorize(status);
    match status {
        200 | 301..=303 | 307 | 308 => assert!(matches!(verdict, oura::Check::Pass(_))),
        400 => assert!(matches!(verdict, oura::Check::Fail(_))),
        _ => assert!(matches!(verdict, oura::Check::Unknown(_)), "{status}"),
    }
}

/// `oura configure`'s secret check reads "invalid_client" (or a 401) as wrong
/// credentials and only "invalid_grant" with a 400 as right ones.
#[hegel::test]
fn configure_reads_the_token_answer(tc: TestCase) {
    let status = tc.draw(gs::integers::<u16>().min_value(100).max_value(599));
    let error = tc.draw(gs::optional(hegel::one_of!(gs::sampled_from(&["invalid_client", "invalid_grant", "invalid_request"][..]).map(|s: &str| s.to_string()), gs::text())));
    let body = error.as_ref().map(|e| json!({"error": e}));
    let verdict = oura::interpret_token_probe(status, body.as_ref());
    let expected = match (status, error.as_deref()) {
        (_, Some("invalid_client")) | (401, _) => oura::ClientProbe::Rejected,
        (400, Some("invalid_grant")) => oura::ClientProbe::Accepted,
        _ => return assert!(matches!(verdict, oura::ClientProbe::Unclear(_))),
    };
    assert_eq!(verdict, expected);
}

// ---------------------------------------------------------------------------
// Eight Sleep
// ---------------------------------------------------------------------------

/// Builds an Eight Sleep day with a main session (and a nap listed first or
/// after it) whose stageSummary holds the given durations.
fn eight_day_with_session(sleep: u32, awake: u32, out: u32, heart: &[Option<u32>], bed: &[u32], nap_first: bool) -> Value {
    let total = sleep + awake + out;
    let main = json!({
        "id": "main",
        "stageSummary": {"totalDuration": total, "sleepDuration": sleep, "awakeDuration": awake, "outDuration": out, "awakeBeforeSleepDuration": awake / 2},
        "timeseries": {
            "heartRate": heart.iter().map(|v| json!(["2026-10-18T04:00:00.000Z", v.map(Value::from).unwrap_or(json!("None"))])).collect::<Vec<_>>(),
            "tempBedC": bed.iter().map(|v| json!(["2026-10-18T04:00:00.000Z", v])).collect::<Vec<_>>(),
        },
    });
    let nap = json!({"id": "nap", "stageSummary": {"sleepDuration": 1, "awakeDuration": 2, "outDuration": 3, "totalDuration": 6}, "timeseries": {"heartRate": [["2026-10-18T19:00:00.000Z", 1]]}});
    json!({"day": "2026-10-18", "mainSessionId": "main", "presenceDuration": total + 7, "sleepDuration": sleep + 7,
           "sessions": if nap_first { json!([nap, main]) } else { json!([main, nap]) }})
}

fn eight_night(day: Value) -> Night {
    eight::summarize(&json!({"days": [day]}), &[d("2026-10-18")]).unwrap().remove(&d("2026-10-18")).unwrap()
}

/// An Eight Sleep night's durations come from its main session: asleep,
/// awake in bed and out of bed kept apart, adding up to time in bed.
#[hegel::test]
fn eight_durations_come_from_the_main_session(tc: TestCase) {
    let (sleep, awake, out) = (tc.draw(gs::integers::<u32>().max_value(50_000)), tc.draw(gs::integers::<u32>().max_value(10_000)), tc.draw(gs::integers::<u32>().max_value(10_000)));
    let n = eight_night(eight_day_with_session(sleep, awake, out, &[], &[], tc.draw(gs::booleans())));
    assert_eq!(n.source_id.as_deref(), Some("main"));
    assert_eq!(
        (n.asleep_s, n.awake_s, n.out_of_bed_s, n.time_in_bed_s, n.latency_s),
        (Some(f64::from(sleep)), Some(f64::from(awake)), Some(f64::from(out)), Some(f64::from(sleep + awake + out)), Some(f64::from(awake / 2)))
    );
}

/// Lowest heart rate is the main session's lowest sample, and bed temperature
/// the mean of its samples; "None" samples are skipped.
#[hegel::test]
fn eight_lowest_heart_rate_and_temperatures_come_from_samples(tc: TestCase) {
    let heart: Vec<Option<u32>> = tc.draw(gs::vecs(gs::optional(gs::integers::<u32>().min_value(30).max_value(200))).max_size(20));
    let bed: Vec<u32> = tc.draw(gs::vecs(gs::integers::<u32>().min_value(10).max_value(45)).max_size(20));
    let n = eight_night(eight_day_with_session(25_000, 2_000, 500, &heart, &bed, tc.draw(gs::booleans())));
    assert_eq!(n.hr_lowest_bpm, heart.iter().flatten().min().map(|v| f64::from(*v)));
    let mean = (!bed.is_empty()).then(|| bed.iter().map(|v| f64::from(*v)).sum::<f64>() / bed.len() as f64);
    assert_eq!(n.bed_temp_c, mean);
}

/// Heart rate, HRV and breathing are tonight's (`current`), never Eight
/// Sleep's rolling baseline (`average`); an unknown `current` is unknown
/// unless a day-level value exists.
#[hegel::test]
fn eight_uses_tonights_values_not_the_baseline(tc: TestCase) {
    let (tonight, baseline, day_level) = (tc.draw(gs::integers::<u32>().max_value(300)), tc.draw(gs::integers::<u32>().max_value(300)), tc.draw(gs::optional(gs::integers::<u32>().max_value(300))));
    let current = if tc.draw(gs::booleans()) { json!(tonight) } else { tc.draw(gs::sampled_from(&[Value::Null, json!("None")][..])) };
    let key = tc.draw(gs::sampled_from(&["heartRate", "hrv", "respiratoryRate"][..]));
    let mut day = json!({"day": "2026-10-18", "sleepQualityScore": {key: {"current": current, "average": baseline}}});
    if let Some(v) = day_level {
        day[key] = json!(v);
    }
    let n = eight_night(day);
    let got = match key {
        "heartRate" => n.hr_avg_bpm,
        "hrv" => n.hrv_avg_ms,
        _ => n.breath_avg_per_min,
    };
    let want = current.as_f64().or(day_level.map(f64::from));
    assert_eq!(got, want);
}

/// Without a session, Eight Sleep's day-level totals fill in, and awake and
/// out-of-bed time are unknown rather than guessed.
#[hegel::test]
fn eight_day_totals_fill_in_without_a_session(tc: TestCase) {
    let (presence, sleep) = (tc.draw(gs::integers::<u32>().max_value(60_000)), tc.draw(gs::integers::<u32>().max_value(60_000)));
    let n = eight_night(json!({"day": "2026-10-18", "presenceDuration": presence, "sleepDuration": sleep}));
    assert_eq!((n.time_in_bed_s, n.asleep_s, n.awake_s, n.out_of_bed_s), (Some(f64::from(presence)), Some(f64::from(sleep)), None, None));
}

/// When Eight Sleep lists a day twice, the longer sleep wins whatever the
/// order, and a known duration (even 0) beats an unknown one.
#[hegel::test]
fn duplicate_eight_days_keep_the_longer_known_sleep(tc: TestCase) {
    let a = tc.draw(gs::optional(gs::integers::<u32>().max_value(50_000)));
    let b = tc.draw(gs::optional(gs::integers::<u32>().max_value(50_000)));
    tc.assume(a != b);
    let mk = |sleep: Option<u32>, score: u32| json!({"day": "2026-10-18", "sleepDuration": sleep, "score": score});
    let winner = if a.map(i64::from).unwrap_or(-1) > b.map(i64::from).unwrap_or(-1) { 1.0 } else { 2.0 };
    for days in [json!([mk(a, 1), mk(b, 2)]), json!([mk(b, 2), mk(a, 1)])] {
        let n = eight::summarize(&json!({"days": days}), &[d("2026-10-18")]).unwrap().remove(&d("2026-10-18")).unwrap();
        assert_eq!(n.sleep_score, Some(winner));
    }
}

/// The order Eight Sleep returns days in never changes the nights.
#[hegel::test]
fn eight_order_never_changes_the_nights(tc: TestCase) {
    let mut days: Vec<Value> = tc.draw(gs::vecs(eight_days()).max_size(8));
    let wanted: Vec<NaiveDate> = days.iter().filter_map(|x| x["day"].as_str()).map(d).collect();
    let before = eight::summarize(&json!({"days": days}), &wanted).unwrap();
    days.reverse();
    let rotate = tc.draw(gs::integers::<usize>().max_value(days.len().max(1) - 1));
    let by = rotate.min(days.len().saturating_sub(1));
    days.rotate_left(by);
    assert_eq!(eight::summarize(&json!({"days": days}), &wanted).unwrap(), before);
}

/// An Eight Sleep night is "still processing" when either `processing` or
/// `incomplete` says so, and unknown when neither is a true/false value.
#[hegel::test]
fn either_eight_flag_marks_a_night_as_processing(tc: TestCase) {
    let flags = [None, Some(json!(true)), Some(json!(false)), Some(Value::Null), Some(json!("true"))];
    let (processing, incomplete) = (tc.draw(gs::sampled_from(&flags[..])), tc.draw(gs::sampled_from(&flags[..])));
    let mut day = json!({"day": "2026-10-18"});
    for (key, v) in [("processing", &processing), ("incomplete", &incomplete)] {
        if let Some(v) = v {
            day[key] = v.clone();
        }
    }
    let b = |v: &Option<Value>| v.as_ref().and_then(Value::as_bool);
    let want = match (b(&processing), b(&incomplete)) {
        (None, None) => None,
        (p, i) => Some(p == Some(true) || i == Some(true)),
    };
    assert_eq!(eight_night(day).processing, want);
}

/// Eight Sleep temperatures and lowest heart rate are never infinite, even
/// when samples are large enough to overflow a sum.
#[hegel::test]
fn eight_sample_metrics_are_never_infinite(tc: TestCase) {
    let samples: Vec<f64> = tc.draw(gs::vecs(hegel::one_of!(gs::just(f64::MAX), gs::just(1e308), finite())).min_size(1).max_size(5));
    let series: Vec<Value> = samples.iter().map(|v| json!(["2026-10-18T04:00:00.000Z", v])).collect();
    let day = json!({"day": "2026-10-18", "mainSessionId": "m", "sessions": [{"id": "m", "timeseries": {"heartRate": series, "tempBedC": series, "tempRoomC": series}}]});
    let n = eight_night(day);
    for v in [n.bed_temp_c, n.room_temp_c, n.hr_lowest_bpm] {
        assert!(v.is_none_or(f64::is_finite), "{v:?}");
    }
}

/// Eight Sleep reports one night a day, so how many other periods it had is
/// unknown, never zero.
#[hegel::test]
fn eight_never_claims_a_count_of_other_periods(tc: TestCase) {
    let day = tc.draw(eight_days());
    let wanted: Vec<NaiveDate> = views::doc_day(&day).into_iter().collect();
    for n in eight::summarize(&json!({"days": [day]}), &wanted).unwrap().values() {
        assert_eq!(n.other_periods, None);
    }
}

/// An Eight Sleep session expiry is used as given, rounded up and capped at
/// ten years; zero, negative or missing means one hour; nothing overflows.
#[hegel::test]
fn eight_session_expiry_is_bounded(tc: TestCase) {
    let now = tc.draw(gs::integers::<i64>().min_value(0).max_value(4_000_000_000));
    let expires = tc.draw(gs::optional(finite()));
    let mut v = json!({"access_token": "t", "userId": "u1"});
    if let Some(e) = expires {
        v["expires_in"] = json!(e);
    }
    let s = eight::session_from_response(&v, "a@b.c", now).unwrap();
    let want = match expires {
        Some(e) if e > 0.0 => now.saturating_add(e.min(10.0 * 365.0 * 86400.0).ceil() as i64),
        _ => now + 3600,
    };
    assert_eq!(s.expires_at, want);
}

/// An Eight Sleep user id that could change a URL path (slashes, dots,
/// anything but letters, digits, - and _) is refused at login.
#[hegel::test]
fn eight_user_ids_that_could_change_a_url_are_refused(tc: TestCase) {
    let id = tc.draw(gs::text().min_size(1));
    // "None" is how Eight Sleep writes a missing value, so it counts as no id.
    let ok = id != "None" && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let result = eight::session_from_response(&json!({"access_token": "t", "userId": id}), "a@b.c", 0);
    assert_eq!(result.is_ok(), ok, "{id:?}");
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Zero shows as "0m" and unknown as "—": the two are never confused.
#[hegel::test]
fn zero_and_unknown_are_never_confused(tc: TestCase) {
    let minutes = tc.draw(gs::integers::<u32>().max_value(59));
    let known = Night { asleep_s: Some(f64::from(minutes) * 60.0), ..Night::default() };
    let out = night::render_night(d("2026-10-18"), ny(), &[(Source::Oura, Slot::Record { night: Box::new(known) }), (Source::EightSleep, Slot::Record { night: Box::new(Night::default()) })]);
    assert_eq!(field(&out, "asleep").split_whitespace().collect::<Vec<_>>(), vec![format!("{minutes}m").as_str(), "—"]);
}

/// Oura times show the clock where you were (with its offset); Eight Sleep
/// times, sent in UTC, are converted to your zone.
#[hegel::test]
fn each_source_shows_times_its_own_way(tc: TestCase) {
    let offset = tc.draw(offsets());
    let minutes = tc.draw(gs::integers::<i64>().min_value(0).max_value(24 * 60));
    let t = (chrono::Utc.from_utc_datetime(&d("2026-10-18").and_hms_opt(0, 0, 0).unwrap()) + Duration::minutes(minutes)).with_timezone(&offset);
    let n = Night { bedtime_start: Some(t), bedtime_end: Some(t + Duration::hours(1)), ..Night::default() };
    let out = night::render_night(d("2026-10-18"), ny(), &[(Source::Oura, Slot::Record { night: Box::new(n.clone()) }), (Source::EightSleep, Slot::Record { night: Box::new(n) })]);
    let line = field(&out, "in bed");
    let own = format!("{} → {}", t.format("%H:%M"), (t + Duration::hours(1)).format("%H:%M"));
    let home = format!("{} → {}", t.with_timezone(&ny()).format("%H:%M"), (t + Duration::hours(1)).with_timezone(&ny()).format("%H:%M"));
    assert!(line.starts_with(&own) && line.ends_with(&home), "{line}");
}

/// Every Oura clock time shown is in the display zone's offset or labelled
/// with its own, including nights that start in one offset and end in another.
#[hegel::test]
fn every_shown_time_is_home_time_or_labelled(tc: TestCase) {
    let p = tc.draw(period());
    let tz = zone(tc.draw(gs::sampled_from(&ZONES[..])));
    let (start, end) = (p.start, p.end());
    let out = night::local_span(Some(&start), Some(&end), tz, " → ").unwrap();
    let away = |t: &DateTime<FixedOffset>| *t.offset() != t.with_timezone(&tz).offset().fix();
    for t in [start, end] {
        if away(&t) {
            assert!(out.contains(&format!("({})", t.format("%:z"))), "{t} unlabelled in {out:?} ({tz})");
        }
    }
    if !away(&start) && !away(&end) {
        assert!(!out.contains('('), "{out:?}");
    }
}

/// The single-day view shows every field Oura sent (except its id), so data
/// Oura adds later still appears.
#[hegel::test]
fn the_day_view_shows_every_field(tc: TestCase) {
    let keys: Vec<String> = tc.draw(gs::vecs(gs::from_regex(r"[a-z][a-z_]{0,15}")).max_size(10).unique(true));
    let mut doc = serde_json::Map::new();
    for k in &keys {
        doc.insert(k.clone(), tc.draw(field_values()));
    }
    let out = views::render_detail(&[], &Value::Object(doc), ny());
    for k in keys.iter().filter(|k| *k != "id") {
        assert!(out.lines().any(|l| l.trim_start().starts_with(&k.replace('_', " "))), "{k} missing from\n{out}");
    }
}

/// Sample summaries count only the samples present and show their real
/// average, minimum and maximum.
#[hegel::test]
fn sample_summaries_use_only_present_samples(tc: TestCase) {
    let items: Vec<Option<u32>> = tc.draw(gs::vecs(gs::optional(gs::integers::<u32>().min_value(30).max_value(200))).min_size(1).max_size(30));
    let present: Vec<f64> = items.iter().flatten().map(|v| f64::from(*v)).collect();
    tc.assume(!present.is_empty());
    let out = views::render_detail(&[], &json!({"heart_rate": {"items": items, "interval": 300}}), ny());
    let avg = present.iter().sum::<f64>() / present.len() as f64;
    let (min, max) = (present.iter().cloned().fold(f64::INFINITY, f64::min), present.iter().cloned().fold(f64::NEG_INFINITY, f64::max));
    assert!(out.contains(&format!("{} samples every 300s · avg {avg:.0} bpm · min {min:.0} · max {max:.0}", present.len())), "{out}");
}

/// Big numbers get thousands separators that read back as the same number.
#[hegel::test]
fn thousands_separators_read_back_as_the_number(tc: TestCase) {
    let n = tc.draw(gs::integers::<i64>().min_value(-1_000_000_000_000).max_value(1_000_000_000_000));
    let s = views::thousands(n as f64);
    assert_eq!(s.replace(',', ""), n.to_string());
    let groups: Vec<&str> = s.trim_start_matches('-').split(',').collect();
    assert!(!groups[0].is_empty() && groups[0].len() <= 3 && groups[1..].iter().all(|g| g.len() == 3), "{s}");
}

/// A table has a header and one row per document.
#[hegel::test]
fn tables_have_one_row_per_document(tc: TestCase) {
    let view = views::view(tc.draw(gs::sampled_from(&["readiness", "sleep", "activity", "stress", "workouts", "tags"][..]))).unwrap();
    let docs: Vec<Value> = tc.draw(gs::vecs(hegel::extras::serde_json::values()).max_size(10));
    assert_eq!(views::render_table(&view, &docs, ny()).lines().count(), docs.len() + 1);
}

/// The one-day dashboard never says "none" or "off" for data it couldn't fetch.
#[hegel::test]
fn the_dashboard_never_calls_missing_data_empty(tc: TestCase) {
    let (collection, label) = tc.draw(gs::sampled_from(&[("workout", "workouts"), ("session", "sessions"), ("rest_mode_period", "rest mode"), ("tag", "tags"), ("enhanced_tag", "tags"), ("daily_stress", "stress"), ("sleep", "sleep"), ("daily_readiness", "readiness")][..]));
    let errors = BTreeMap::from([(collection.to_string(), "fetch failed".to_string())]);
    let out = views::render_day(d("2026-10-18"), ny(), &BTreeMap::new(), &errors);
    assert!(field(&out, label).contains("unavailable"), "{label}: {}", field(&out, label));
}

/// The dashboard's sleep line is the main confirmed period, with confirmed
/// naps counted, never deleted or rejected ones.
#[hegel::test]
fn the_dashboard_sleep_line_is_the_main_confirmed_period(tc: TestCase) {
    let hours = tc.draw(gs::integers::<u32>().min_value(2).max_value(10));
    let day = d("2026-10-18");
    let mk = |id: &str, kind: &str, h: u32| json!({"id": id, "day": day.to_string(), "type": kind, "total_sleep_duration": h * 3600});
    let docs = vec![mk("nap", "late_nap", 1), mk("deleted", "deleted", hours + 2), mk("rejected", "rest", hours + 3), mk("main", "long_sleep", hours)];
    let out = views::render_day(day, ny(), &BTreeMap::from([("sleep".to_string(), docs)]), &BTreeMap::new());
    let line = field(&out, "sleep");
    assert!(line.contains(&format!("{hours}h 00m asleep")) && line.contains("+1 other period(s)"), "{line}");
}

/// The week table has one row per day for each source that could be read,
/// and a closing line naming each source that couldn't.
#[hegel::test]
fn the_week_table_has_a_row_per_day_per_readable_source(tc: TestCase) {
    let start = tc.draw(any_day());
    let days = days_inclusive(start, start + Duration::days(tc.draw(gs::integers::<i64>().min_value(0).max_value(30))));
    let oura_ok = tc.draw(gs::booleans());
    let eight_ok = tc.draw(gs::booleans());
    let nights = |ok: bool| -> night::SourceNights { if ok { Ok(BTreeMap::new()) } else { Err("sentinel".into()) } };
    let out = night::render_table(&days, ny(), &[(Source::Oura, nights(oura_ok)), (Source::EightSleep, nights(eight_ok))]);
    let readable = usize::from(oura_ok) + usize::from(eight_ok);
    let failed = 2 - readable;
    assert_eq!(out.lines().count(), 2 + days.len() * readable + failed, "{out}");
    assert_eq!(out.lines().filter(|l| l.ends_with(": sentinel")).count(), failed);
}

/// The heart-rate table has a row per bucket, and an hour that happens twice
/// (clocks falling back) is labelled with its offset each time.
#[hegel::test]
fn the_heart_rate_table_has_a_row_per_bucket(tc: TestCase) {
    let samples: Vec<Value> = tc.draw(gs::vecs(hr_samples()).max_size(60));
    let tz = zone(tc.draw(gs::sampled_from(&ZONES[..])));
    let buckets = views::hr_buckets(&samples, tz, true);
    let out = views::render_hr(&buckets);
    assert_eq!(out.lines().count(), buckets.len() + 1);
    let mut seen: BTreeMap<(NaiveDate, u32), usize> = BTreeMap::new();
    for (_, hour) in buckets.keys() {
        let h = hour.unwrap();
        *seen.entry((h.date_naive(), h.hour())).or_default() += 1;
    }
    for (line, ((_, hour), _)) in out.lines().skip(1).zip(&buckets) {
        let h = hour.unwrap();
        let twice = seen[&(h.date_naive(), h.hour())] > 1;
        assert_eq!(line.contains(&format!("({})", h.format("%:z"))), twice, "{line}");
    }
}

// ---------------------------------------------------------------------------
// Heart rate by hour and day
// ---------------------------------------------------------------------------

/// Heart-rate buckets count every sample once by local day (and hour), with
/// the right minimum, maximum, average and sources; the two 01:00 hours
/// when clocks fall back stay apart.
#[hegel::test]
fn heart_rate_buckets_count_every_sample_once(tc: TestCase) {
    let samples: Vec<Value> = tc.draw(gs::vecs(hr_samples()).max_size(80));
    let by_hour = tc.draw(gs::booleans());
    let tz = zone(tc.draw(gs::sampled_from(&ZONES[..])));
    let got = views::hr_buckets(&samples, tz, by_hour);
    type Bucket = (usize, f64, f64, f64, BTreeMap<String, usize>);
    let mut model: BTreeMap<views::HrKey, Bucket> = BTreeMap::new();
    for s in &samples {
        let Some(bpm) = s["bpm"].as_f64() else { continue };
        let t = ts(s["timestamp"].as_str().unwrap()).with_timezone(&tz);
        // The local clock hour, in the offset the sample was taken in.
        let hour = by_hour.then(|| t.date_naive().and_hms_opt(t.hour(), 0, 0).unwrap().and_local_timezone(t.offset().fix()).unwrap());
        let e = model.entry((t.date_naive(), hour)).or_insert((0, f64::INFINITY, f64::NEG_INFINITY, 0.0, BTreeMap::new()));
        e.0 += 1;
        e.1 = e.1.min(bpm);
        e.2 = e.2.max(bpm);
        e.3 += bpm;
        *e.4.entry(s["source"].as_str().unwrap().to_string()).or_default() += 1;
    }
    assert_eq!(got.len(), model.len());
    for (key, (count, min, max, sum, sources)) in model {
        let s = &got[&key];
        assert_eq!((s.samples, s.min, s.max, &s.by_source), (count, min, max, &sources));
        assert!((s.avg().unwrap() - sum / count as f64).abs() < 1e-9);
    }
}

/// Every heart-rate hour starts on the hour where it happened (minute 0 in
/// its own offset), even where clocks change by 30 minutes.
#[hegel::test]
fn heart_rate_hours_start_on_the_hour(tc: TestCase) {
    let samples: Vec<Value> = tc.draw(gs::vecs(hr_samples()).max_size(60));
    let tz = zone(tc.draw(gs::sampled_from(&ZONES[..])));
    for (day, hour) in views::hr_buckets(&samples, tz, true).keys() {
        let h = hour.unwrap();
        assert_eq!((h.minute(), h.second(), h.nanosecond(), h.date_naive()), (0, 0, 0, *day), "{h}");
    }
}

/// Heart-rate averages are never infinite and never outside the hour's
/// minimum and maximum.
#[hegel::test]
fn heart_rate_averages_stay_between_min_and_max(tc: TestCase) {
    let bpms: Vec<f64> = tc.draw(gs::vecs(hegel::one_of!(gs::just(f64::MAX), gs::just(0.1), finite())).min_size(1).max_size(6));
    let samples: Vec<Value> = bpms.iter().map(|b| json!({"bpm": b, "source": "rest", "timestamp": "2026-10-18T04:00:00+00:00"})).collect();
    for s in views::hr_buckets(&samples, ny(), false).values() {
        if let Some(a) = s.avg() {
            assert!(a.is_finite() && s.min <= a && a <= s.max, "{a} outside {}..{}", s.min, s.max);
        }
    }
}

/// Rewriting heart-rate timestamps in another UTC offset moves no sample to
/// another bucket.
#[hegel::test]
fn heart_rate_buckets_ignore_how_times_are_written(tc: TestCase) {
    let samples: Vec<Value> = tc.draw(gs::vecs(hr_samples()).max_size(40));
    let other = tc.draw(offsets());
    let by_hour = tc.draw(gs::booleans());
    let moved: Vec<Value> = samples.iter().map(|s| {
        let mut s = s.clone();
        s["timestamp"] = json!(ts(s["timestamp"].as_str().unwrap()).with_timezone(&other).to_rfc3339());
        s
    }).collect();
    assert_eq!(views::hr_buckets(&samples, ny(), by_hour), views::hr_buckets(&moved, ny(), by_hour));
}

// ---------------------------------------------------------------------------
// The log and the network allowlist
// ---------------------------------------------------------------------------

const SENSITIVE: [&str; 9] = ["code", "state", "client_id", "client_secret", "access_token", "refresh_token", "password", "next_token", "code_challenge"];

/// Builds a URL with sensitive and ordinary query parameters.
#[hegel::composite]
fn logged_urls(tc: &TestCase) -> (reqwest::Url, Vec<String>, Vec<(String, String)>) {
    let secrets: Vec<(&str, String)> = tc.draw(gs::vecs(hegel::tuples!(gs::sampled_from(&SENSITIVE[..]), gs::text().min_size(1).map(|s: String| format!("SeCrEt{s}")))).min_size(1).max_size(4));
    let plain: Vec<(&str, String)> = tc.draw(gs::vecs(hegel::tuples!(gs::sampled_from(&["start_date", "end_date", "scope", "redirect_uri", "tz"][..]), gs::text())).max_size(4));
    let mut pairs: Vec<(String, String)> = secrets.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    pairs.extend(plain.iter().map(|(k, v)| (k.to_string(), v.clone())));
    let url = reqwest::Url::parse_with_params("https://api.ouraring.com/v2/usercollection/sleep", &pairs).unwrap();
    (url, secrets.into_iter().map(|(_, v)| v).collect(), plain.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// Secrets never reach the log when their parameter name is in another letter
/// case or they sit in the URL's fragment.
#[hegel::test]
fn secrets_in_any_case_or_fragment_never_reach_the_log(tc: TestCase) {
    let (mut url, _, _) = tc.draw(logged_urls());
    let upper = tc.draw(gs::booleans());
    let pairs: Vec<(String, String)> = url.query_pairs().map(|(k, v)| (if upper { k.to_uppercase() } else { k.into_owned() }, v.into_owned())).collect();
    url.query_pairs_mut().clear().extend_pairs(pairs);
    if tc.draw(gs::booleans()) {
        url.set_fragment(Some(&format!("access_token=SeCrEt{}", tc.draw(gs::text()))));
    }
    let logged = log::redact_url(&url);
    assert!(!logged.contains("SeCrEt"), "{logged}");
}

/// No login code, state, client id or secret, token or password reaches the
/// log, in any encoding.
#[hegel::test]
fn secrets_never_reach_the_log(tc: TestCase) {
    let (url, secrets, _) = tc.draw(logged_urls());
    let logged = log::redact_url(&url);
    for secret in &secrets {
        let form: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
        assert!(!logged.contains(secret.as_str()) && !logged.contains(&form) && !logged.contains("SeCrEt"), "{secret:?} leaked in {logged}");
    }
}

/// Everything else in a logged URL (dates, scopes, time zone) is kept
/// exactly, so the log shows what was asked for.
#[hegel::test]
fn ordinary_parameters_are_logged_exactly(tc: TestCase) {
    let (url, _, plain) = tc.draw(logged_urls());
    let logged = reqwest::Url::parse(&log::redact_url(&url)).unwrap();
    let kept: Vec<(String, String)> = logged.query_pairs().into_owned().filter(|(k, _)| !SENSITIVE.contains(&k.as_str())).collect();
    assert_eq!(kept, plain);
}

/// A user name or password inside a URL never reaches the log, and redacting
/// twice changes nothing.
#[hegel::test]
fn url_credentials_never_reach_the_log(tc: TestCase) {
    let (mut url, _, _) = tc.draw(logged_urls());
    let user = tc.draw(gs::from_regex(r"UsEr[A-Za-z0-9]{1,10}"));
    url.set_username(&user).unwrap();
    url.set_password(Some(&user)).unwrap();
    let once = log::redact_url(&url);
    assert!(!once.contains("UsEr"), "{once}");
    assert_eq!(log::redact_url(&reqwest::Url::parse(&once).unwrap()), once);
}

/// Requests only go to Oura's and Eight Sleep's API hosts over HTTPS on the
/// default port; any other host, scheme, port or embedded login is refused.
#[hegel::test]
fn requests_only_go_to_the_known_hosts(tc: TestCase) {
    let host = tc.draw(hegel::one_of!(gs::sampled_from(http::ALLOWED_HOSTS).map(|h: &str| h.to_string()), gs::domains()));
    let scheme = tc.draw(gs::sampled_from(&["https", "http"][..]));
    let port = tc.draw(gs::optional(gs::integers::<u16>().min_value(1)));
    let user = tc.draw(gs::optional(gs::from_regex(r"[a-z]{1,8}")));
    let url = format!("{scheme}://{}{host}{}/v2/x", user.as_ref().map(|u| format!("{u}@")).unwrap_or_default(), port.map(|p| format!(":{p}")).unwrap_or_default());
    let Ok(parsed) = reqwest::Url::parse(&url) else { return };
    // Host names are case-insensitive; compare the normalized one.
    let allowed = scheme == "https" && parsed.host_str().is_some_and(|h| http::ALLOWED_HOSTS.contains(&h)) && parsed.port().is_none() && user.is_none();
    assert_eq!(http::check_destination(&parsed).is_ok(), allowed, "{url}");
}

/// A server's error message is shown short (at most 160 characters) and on
/// one line, whatever the body contains.
#[hegel::test]
fn server_error_messages_are_short_and_single_line(tc: TestCase) {
    let key = tc.draw(gs::sampled_from(&["error_description", "message", "detail", "error"][..]));
    let text = tc.draw(gs::text());
    let body = json!({key: text}).to_string();
    if let Some(m) = http::short_message(body.as_bytes()) {
        assert!(m.chars().count() <= 160 && !m.chars().any(char::is_control), "{m:?}");
        assert!(text.chars().filter(|c| !c.is_control()).collect::<String>().starts_with(&m), "{m:?} is not the start of {text:?}");
    }
}

/// A blank error field never hides a message in a later field: somno shows
/// the first field that has text.
#[hegel::test]
fn a_blank_error_field_never_hides_the_message(tc: TestCase) {
    const KEYS: [&str; 4] = ["error_description", "message", "detail", "error"];
    let values: Vec<Option<String>> = (0..4).map(|_| tc.draw(gs::optional(hegel::one_of!(gs::just(String::new()), gs::just("  ".to_string()), gs::text())))).collect();
    let mut body = json!({});
    for (k, v) in KEYS.iter().zip(&values) {
        if let Some(v) = v {
            body[*k] = json!(v);
        }
    }
    let shown = |v: &String| -> Option<String> {
        let clean: String = v.chars().filter(|c| !c.is_control()).take(160).collect();
        (!clean.trim().is_empty()).then_some(clean)
    };
    assert_eq!(http::short_message(body.to_string().as_bytes()), values.iter().flatten().find_map(shown));
}

// ---------------------------------------------------------------------------
// Robustness: whatever the providers send
// ---------------------------------------------------------------------------

const OURA_KEYS: &[&str] = &[
    "id", "day", "type", "bedtime_start", "bedtime_end", "time_in_bed", "total_sleep_duration", "deep_sleep_duration",
    "rem_sleep_duration", "light_sleep_duration", "awake_time", "latency", "efficiency", "average_heart_rate",
    "lowest_heart_rate", "average_hrv", "average_breath", "restless_periods", "sleep_phase_30_sec", "movement_30_sec",
];

/// A valid Oura sleep document with one field corrupted or removed, so the
/// rest still reaches the deeper code.
#[hegel::composite]
fn corrupted_periods(tc: &TestCase) -> Value {
    let mut doc = tc.draw(period()).doc();
    let key = tc.draw(gs::sampled_from(OURA_KEYS));
    if tc.draw(gs::booleans()) {
        doc.as_object_mut().unwrap().remove(key);
    } else {
        doc[key] = tc.draw(field_values());
    }
    doc
}

/// Whatever one field of a sleep record holds, no view crashes, and the night
/// shown never invents an asleep time.
#[hegel::test]
fn a_corrupted_sleep_record_never_crashes_a_view(tc: TestCase) {
    let doc = tc.draw(corrupted_periods());
    let width = tc.draw(gs::integers::<usize>().min_value(1).max_value(300));
    let day = views::doc_day(&doc).unwrap_or_else(|| d("2026-10-18"));
    if let Some(v) = oura::summarize(std::slice::from_ref(&doc), &[], &[], &[day]).get(&day).and_then(|n| n.asleep_s) {
        assert_eq!(doc["total_sleep_duration"].as_f64(), Some(v));
    }
    let _ = somno::stages::render_period(&doc, width, true);
    let _ = period_json(&doc);
    let _ = views::render_detail(&[], &doc, ny());
    let _ = views::render_table(&views::view("sleep").unwrap(), std::slice::from_ref(&doc), ny());
    let _ = views::render_day(day, ny(), &BTreeMap::from([("sleep".to_string(), vec![doc.clone()])]), &BTreeMap::new());
}

/// Any JSON at all as a provider document never crashes a summary or a view.
#[hegel::test]
fn any_json_never_crashes_a_summary_or_view(tc: TestCase) {
    let doc = tc.draw(hegel::extras::serde_json::values());
    let day = d("2026-10-18");
    let _ = oura::summarize(std::slice::from_ref(&doc), std::slice::from_ref(&doc), std::slice::from_ref(&doc), &[day]);
    let _ = eight::summarize(&json!({"days": [doc.clone()]}), &[day]);
    let _ = eight::summarize(&doc, &[day]);
    let _ = somno::stages::render_period(&doc, 80, true);
    let _ = views::render_detail(&[], &doc, ny());
    let _ = views::render_day(day, ny(), &BTreeMap::from([("sleep".to_string(), vec![doc.clone()]), ("daily_activity".to_string(), vec![doc])]), &BTreeMap::new());
}

/// Any Eight Sleep day shape (numbers, nulls, "None", any JSON in any field)
/// never crashes, and a response without a days list is an error.
#[hegel::test]
fn any_eight_day_shape_never_crashes(tc: TestCase) {
    let days: Vec<Value> = tc.draw(gs::vecs(eight_days()).max_size(6));
    let wanted: Vec<NaiveDate> = days.iter().filter_map(|x| x["day"].as_str()).map(d).collect();
    let _ = eight::summarize(&json!({"days": days}), &wanted).unwrap();
    let junk = tc.draw(hegel::extras::serde_json::values());
    assert_eq!(eight::summarize(&junk, &wanted).is_ok(), junk.get("days").and_then(Value::as_array).is_some());
}

// ---------------------------------------------------------------------------
// Scale probe: very large inputs, once
// ---------------------------------------------------------------------------

/// A year of data, a week of 30-second stages, and a month of heart-rate
/// samples process in well under a second each: nothing is accidentally
/// quadratic.
#[test]
fn large_inputs_stay_fast() {
    let time = |what: &str, f: &dyn Fn()| {
        let start = std::time::Instant::now();
        f();
        assert!(start.elapsed() < std::time::Duration::from_secs(2), "{what} took {:?}", start.elapsed());
    };
    let days: Vec<NaiveDate> = (0..400).map(|i| d("2025-09-01") + Duration::days(i)).collect();
    let docs: Vec<Value> = days.iter().flat_map(|day| (0..3).map(move |k| json!({"id": format!("{day}-{k}"), "day": day.to_string(), "type": "long_sleep", "total_sleep_duration": 20000 + k}))).collect();
    time("a year of nightly summaries", &|| {
        let _ = oura::summarize(&docs, &docs, &docs, &days);
    });
    time("selecting a year of documents", &|| {
        let _ = views::select("sleep", docs.clone(), days[0], days[399]);
    });
    let codes = "1234".repeat(7 * 2880 / 4);
    let night = json!({"bedtime_start": "2026-10-18T00:00:00Z", "sleep_phase_30_sec": codes, "movement_30_sec": codes});
    time("a week of 30-second stages", &|| {
        let _ = somno::stages::render_period(&night, 120, true);
        let _ = period_json(&night);
    });
    let samples: Vec<Value> = (0..31 * 24 * 12).map(|i| json!({"bpm": 50 + i % 50, "source": "rest", "timestamp": (chrono::Utc.from_utc_datetime(&d("2026-10-01").and_hms_opt(0, 0, 0).unwrap()) + Duration::minutes(5 * i)).to_rfc3339()})).collect();
    time("a month of heart rate by hour", &|| {
        let _ = views::render_hr(&views::hr_buckets(&samples, ny(), true));
    });
    let eight_days: Vec<Value> = days.iter().map(|day| json!({"day": day.to_string(), "sleepDuration": 25000})).collect();
    time("a year of Eight Sleep days", &|| {
        let _ = eight::summarize(&json!({"days": eight_days}), &days);
    });
}

// ---------------------------------------------------------------------------
// Fixes from the Codex test audit (2026-10-21)
// ---------------------------------------------------------------------------

/// A server message that echoes one of the secrets the request sent, anywhere
/// in its text, never shows it in the error or the log line.
#[hegel::test]
fn secrets_a_request_sent_never_come_back_in_an_error(tc: TestCase) {
    // Secrets in a shape that can't appear inside the word "redacted".
    let sent: Vec<String> = tc.draw(gs::vecs(gs::from_regex(r"[A-Z0-9]{8,40}")).min_size(1).max_size(3));
    let echoed = sent[tc.draw(gs::integers::<usize>().max_value(sent.len() - 1))].clone();
    let (before, after) = (tc.draw(gs::from_regex(r"[a-z ]{0,60}")), tc.draw(gs::from_regex(r"[a-z ]{0,60}")));
    let key = tc.draw(gs::sampled_from(&["error_description", "message", "detail", "error"][..]));
    let body = json!({key: format!("{before}{echoed}{after}")}).to_string();
    let refs: Vec<&str> = sent.iter().map(String::as_str).collect();
    let shown = http::error_message(body.as_bytes(), &refs).unwrap_or_default();
    let logged = http::error_summary(body.as_bytes(), &refs);
    for s in &sent {
        assert!(!shown.contains(s.as_str()) && !logged.contains(s.as_str()), "{s} leaked: {shown:?} / {logged:?}");
    }
}

/// A `name=value` pair for a secret name in a server message is redacted
/// even when somno didn't send that value.
#[hegel::test]
fn secret_name_value_pairs_in_server_messages_are_redacted(tc: TestCase) {
    let name = tc.draw(gs::sampled_from(&SENSITIVE[..]));
    let upper = tc.draw(gs::booleans());
    let name = if upper { name.to_uppercase() } else { name.to_string() };
    let value = tc.draw(gs::from_regex(r"[A-Z0-9]{8,40}"));
    let lead = tc.draw(gs::sampled_from(&["invalid ", "bad request: ", ""][..]));
    let body = json!({"error_description": format!("{lead}{name}={value}")}).to_string();
    let shown = http::error_message(body.as_bytes(), &[]).unwrap();
    assert!(!shown.contains(&value) && shown.contains(&format!("{name}=redacted")), "{shown:?}");
}

/// A night whose `mainSessionId` names a session that isn't in the list
/// uses the day's own totals, never another session such as a nap.
#[hegel::test]
fn a_missing_main_session_never_borrows_another_sessions_numbers(tc: TestCase) {
    let day_sleep = tc.draw(gs::integers::<u32>().min_value(3600).max_value(40000));
    let naps: Vec<u32> = tc.draw(gs::vecs(gs::integers::<u32>().min_value(60).max_value(3000)).min_size(1).max_size(3));
    let sessions: Vec<Value> = naps.iter().enumerate().map(|(i, s)| json!({"id": format!("nap{i}"), "stageSummary": {"sleepDuration": s}})).collect();
    let day = json!({"day": "2026-10-18", "mainSessionId": "main", "sleepDuration": day_sleep, "sessions": sessions});
    assert_eq!(eight_night(day).asleep_s, Some(f64::from(day_sleep)));
}

/// Without a `mainSessionId`, the night is the session with the most sleep,
/// wherever it sits in the list.
#[hegel::test]
fn without_a_main_session_id_the_longest_session_is_the_night(tc: TestCase) {
    let sleeps: Vec<u32> = tc.draw(gs::vecs(gs::integers::<u32>().min_value(60).max_value(40000)).min_size(1).max_size(5));
    let sessions: Vec<Value> = sleeps.iter().enumerate().map(|(i, s)| json!({"id": format!("s{i}"), "stageSummary": {"sleepDuration": s}})).collect();
    let n = eight_night(json!({"day": "2026-10-18", "sessions": sessions}));
    assert_eq!(n.asleep_s, sleeps.iter().max().map(|m| f64::from(*m)));
}

/// `hr` for a range keeps exactly the samples whose local day is in it, so
/// a sample at the next day's midnight never lands under the last day.
#[hegel::test]
fn heart_rate_for_a_range_keeps_only_its_local_days(tc: TestCase) {
    let samples: Vec<Value> = tc.draw(gs::vecs(hr_samples()).max_size(60));
    let tz = zone(tc.draw(gs::sampled_from(&ZONES[..])));
    let start = tc.draw(any_day());
    let end = start + Duration::days(tc.draw(gs::integers::<i64>().min_value(0).max_value(3)));
    let kept = views::samples_within(samples.clone(), tz, start, end);
    let local_day = |s: &Value| ts(s["timestamp"].as_str().unwrap()).with_timezone(&tz).date_naive();
    let expected: Vec<&Value> = samples.iter().filter(|s| (start..=end).contains(&local_day(s))).collect();
    assert_eq!(kept.iter().collect::<Vec<_>>(), expected);
}

/// "Fell asleep" is never after the bedtime end Oura recorded, even when the
/// stage codes run longer than the period.
#[hegel::test]
fn fell_asleep_is_never_after_the_recorded_bedtime_end(tc: TestCase) {
    let p = tc.draw(period());
    let span = p.epochs() as i64 * 30;
    let cut = tc.draw(gs::integers::<i64>().min_value(0).max_value(span));
    let latency = tc.draw(gs::integers::<i64>().min_value(0).max_value(span));
    let end = p.start + Duration::seconds(cut);
    let mut doc = p.doc();
    doc["bedtime_end"] = json!(end.to_rfc3339());
    let Some(h) = Hypnogram::from_doc(&doc) else { return };
    if let Some(fell) = events(&h, Some(latency as f64)).fell_asleep {
        if latency <= span && fell == p.start + Duration::seconds(latency) {
            assert!(fell <= end, "fell asleep {fell} after bedtime end {end}");
        }
    }
}

/// Each stage's share of time in bed counts epochs with codes Oura didn't
/// define as time in bed.
#[hegel::test]
fn shares_of_time_in_bed_count_undefined_codes(tc: TestCase) {
    let codes = tc.draw(gs::from_regex(r"[1-4x]{1,120}"));
    let doc = json!({"bedtime_start": "2026-10-18T00:00:00+00:00", "sleep_phase_30_sec": codes});
    let out = somno::stages::render_period(&doc, 60, false);
    let total = codes.len() as f64 * 30.0;
    for (label, code) in [("deep", '1'), ("light", '2'), ("REM", '3'), ("awake", '4')] {
        let table = out.split_once("of bed").map(|(_, t)| t).unwrap_or_else(|| panic!("no stage table in\n{out}"));
        let line = table.lines().find(|l| l.starts_with(&format!("  {label:<8}"))).unwrap_or_else(|| panic!("no {label} row in\n{out}"));
        let shown = line.get(19..29).unwrap_or("").trim();
        let secs = codes.chars().filter(|c| *c == code).count() as f64 * 30.0;
        assert_eq!(shown, format!("{:.0}%", 100.0 * secs / total), "{label} in {line:?}");
    }
}

/// Heart-rate samples too large to average show `—`, never NaN.
#[hegel::test]
fn huge_heart_rate_samples_never_show_as_nan(tc: TestCase) {
    let bpms: Vec<f64> = tc.draw(gs::vecs(hegel::one_of!(gs::just(f64::MAX), gs::just(1e308), finite())).min_size(1).max_size(6));
    let samples: Vec<Value> = bpms.iter().map(|b| json!({"bpm": b, "source": "rest", "timestamp": "2026-10-18T04:00:00+00:00"})).collect();
    let out = views::render_hr(&views::hr_buckets(&samples, ny(), false));
    assert!(!out.contains("NaN") && !out.contains("inf"), "{out}");
}

/// A sample series with values counts them in the detail view, even when they
/// are too large to average.
#[hegel::test]
fn a_series_with_samples_never_shows_as_empty(tc: TestCase) {
    let items: Vec<f64> = tc.draw(gs::vecs(hegel::one_of!(gs::just(f64::MAX), gs::just(1e308), finite())).min_size(1).max_size(6));
    let doc = json!({"day": "2026-10-18", "heart_rate": {"interval": 300, "items": items, "timestamp": "2026-10-18T00:00:00+00:00"}});
    let out = views::render_detail(&[], &doc, ny());
    assert!(out.contains(&format!("{} samples", items.len())) && !out.contains("NaN") && !out.contains("inf"), "{out}");
}

/// Every Eight Sleep error reads as a sentence that names the HTTP status, and
/// says what to do where somno knows: log in again, wait, or try later.
#[hegel::test]
fn eight_sleep_errors_say_what_happened_and_what_to_do(tc: TestCase) {
    let status = tc.draw(hegel::one_of!(gs::sampled_from(&[401u16, 403, 404, 429, 500, 503][..]), gs::integers::<u16>().min_value(100).max_value(599)));
    let detail = tc.draw(gs::optional(gs::from_regex(r"[a-z ]{1,40}")));
    let e = http::ApiError::Status { service: "Eight Sleep", status, detail: detail.clone() };
    let text = eight::explain(&e);
    assert!(text.starts_with("Eight Sleep") && text.contains(&format!("HTTP {status}")), "{text}");
    if let Some(d) = &detail {
        assert!(text.ends_with(&format!("Eight Sleep said: {d}")), "{text}");
    }
    let advice = match status {
        401 => Some("somno eight login"),
        429 => Some("Wait a few minutes"),
        500..=599 => Some("Try again later"),
        _ => None,
    };
    if let Some(a) = advice {
        assert!(text.contains(a), "{text}");
    }
}
