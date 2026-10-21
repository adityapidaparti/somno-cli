//! Property-based tests (proptest).
//!
//! Organised by the kinds of property in Hughes, "How to Specify It!" (2019),
//! strongest first:
//! - model-based: compare the code with a small model built by the generator
//!   (stages from runs, the nightly pick from a list of periods, documents in
//!   a range from their intervals, pages, heart-rate buckets, Eight nights)
//! - metamorphic: change the input in a way whose effect is known (shuffle
//!   it, split a range, answer with a stricter server, rewrite timestamps in
//!   another UTC offset, send the same stages at 5-minute resolution)
//! - security postconditions: secrets never reach the log; an OAuth redirect
//!   is accepted only with exactly its state and a non-empty code
//! - round trips, and robustness on corrupted provider JSON
//!
//! Oracles are written independently of the code under test (a direct
//! count, a constructed interval, a text reader), so a shared mistake can't
//! pass both. The first section tests the generators themselves: valid
//! data, and a measured distribution that reaches the interesting cases.
//!
//! The suite's bug-finding is measured by mutation testing:
//! `cargo mutants --re '<core functions>'` plants small bugs and reports any
//! that no test catches. Failing inputs are saved next to this file and
//! replayed first. Deeper run: PROPTEST_CASES=5000 cargo test --test properties

mod common;

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, Timelike};
use chrono_tz::Tz;
use common::*;
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::TestRunner;
use serde_json::{json, Value};
use somno::night::{self, Night, Slot, Source};
use somno::stages::{events, movement, period_json, render_chart, stage_stats, Hypnogram, Stage};
use somno::types::{days_inclusive, fmt_duration, DaySpec};
use somno::{eight, log, oura, views};
use std::collections::{BTreeMap, BTreeSet};

fn ny() -> Tz {
    "America/New_York".parse().unwrap()
}

fn ts(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

fn d(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

/// Adjacent runs of the same stage merged: the run structure the decoder must find.
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

fn confirmed(kind: &str) -> bool {
    !matches!(kind, "deleted" | "rest")
}

/// The value after a row's label in rendered text.
fn field<'a>(out: &'a str, label: &str) -> &'a str {
    out.lines().find_map(|l| l.strip_prefix(label)).unwrap_or_else(|| panic!("no {label} row in\n{out}")).trim()
}

// ======================================================================
// 1. The generators: valid data, and enough of the interesting cases
// ======================================================================

proptest! {
    #[test]
    fn generated_periods_are_internally_consistent(p in arb_period()) {
        let doc = p.doc();
        let codes = doc["sleep_phase_30_sec"].as_str().unwrap();
        prop_assert_eq!(codes.len() as i64 * 30, doc["time_in_bed"].as_i64().unwrap());
        prop_assert_eq!(doc["movement_30_sec"].as_str().unwrap().len(), codes.len());
        let parts = ["deep_sleep_duration", "light_sleep_duration", "rem_sleep_duration"].iter().map(|k| doc[*k].as_i64().unwrap()).sum::<i64>();
        prop_assert_eq!(parts, doc["total_sleep_duration"].as_i64().unwrap());
        let (start, end) = (ts(doc["bedtime_start"].as_str().unwrap()), ts(doc["bedtime_end"].as_str().unwrap()));
        prop_assert_eq!((end - start).num_seconds(), doc["time_in_bed"].as_i64().unwrap());
    }
}

/// Hughes measured that 80% of his generated tests never reached the case
/// under test. This checks the generators reach ours often enough.
#[test]
fn generators_reach_the_interesting_cases() {
    let mut runner = TestRunner::deterministic();
    let n = 2000;
    let mut hits: BTreeMap<&str, usize> = BTreeMap::new();
    let mut hit = |name: &'static str, yes: bool| *hits.entry(name).or_default() += usize::from(yes);
    for _ in 0..n {
        let periods = arb_periods().new_tree(&mut runner).unwrap().current();
        let mut by_day: BTreeMap<NaiveDate, Vec<i64>> = BTreeMap::new();
        for p in periods.iter().filter(|p| p.kind == "long_sleep") {
            by_day.entry(p.day).or_default().push(p.asleep_secs());
        }
        hit("tied long sleeps", by_day.values().any(|v| v.iter().filter(|x| **x == *v.iter().max().unwrap()).count() > 1));
        hit("naps", periods.iter().any(|p| matches!(p.kind, "late_nap" | "sleep")));
        hit("rejected or deleted", periods.iter().any(|p| !confirmed(p.kind)));
        hit("timezone change in a night", periods.iter().any(|p| p.end_offset != *p.start.offset()));
        hit("empty night", periods.iter().any(|p| p.runs.is_empty()));
        hit("wake run at the 5-min threshold", periods.iter().any(|p| merged(&p.runs).iter().any(|(c, n)| *c == '4' && (9..=11).contains(n))));
        hit("latency past the first sleep", periods.iter().any(|p| p.latency_epochs.is_some_and(|l| p.codes().find(|c| c != '4').is_some_and(|f| l > f))));
        hit("east of UTC", periods.iter().any(|p| p.start.offset().local_minus_utc() > 0));
        hit("sub-second start", periods.iter().any(|p| p.start.timestamp_subsec_millis() > 0));
    }
    let pct = |x: usize| 100 * x / n;
    eprintln!("per 100 cases: {}", hits.iter().map(|(k, v)| format!("{k} {}", pct(*v))).collect::<Vec<_>>().join(", "));
    for (name, at_least) in [
        ("tied long sleeps", 10), ("naps", 40), ("rejected or deleted", 30), ("timezone change in a night", 20), ("empty night", 1),
        ("wake run at the 5-min threshold", 15), ("latency past the first sleep", 20), ("east of UTC", 30), ("sub-second start", 30),
    ] {
        assert!(pct(hits[name]) >= at_least, "{name} reached in only {}% of cases", pct(hits[name]));
    }
}

// ======================================================================
// 2. Model-based properties
// ======================================================================

proptest! {
    /// Decoding the stage string recovers the generator's runs, totals,
    /// longest stretches and 5-minute stretch counts.
    #[test]
    fn stages_match_the_model(p in arb_period()) {
        let doc = p.doc();
        let Some(h) = Hypnogram::from_doc(&doc) else {
            prop_assert!(p.runs.is_empty(), "only an empty stage string means no stage data");
            return Ok(());
        };
        prop_assert_eq!(h.epoch_secs, 30);
        let decoded: Vec<(char, usize)> = h.runs().iter().map(|r| (stage_code(r.stage.unwrap()), r.len)).collect();
        let model = merged(&p.runs);
        prop_assert_eq!(&decoded, &model);
        let stats = stage_stats(&h);
        prop_assert_eq!(stats.iter().map(|(s, _)| *s).collect::<Vec<_>>(), Stage::ALL.to_vec());
        for (stage, st) in stats {
            let code = stage_code(stage);
            let mine: Vec<usize> = model.iter().filter(|(c, _)| *c == code).map(|(_, n)| *n).collect();
            prop_assert_eq!(st.seconds, p.secs(code), "total for {:?}", stage);
            prop_assert_eq!(st.longest_seconds, mine.iter().max().copied().unwrap_or(0) as i64 * 30);
            prop_assert_eq!(st.stretches_5min, mine.iter().filter(|n| **n >= 10).count());
        }
    }

    /// Onset, first deep/REM, last sleep, and the wake counted from the
    /// displayed onset to the last sleep, against definitions written
    /// directly on the epoch string.
    #[test]
    fn stage_events_match_the_model(p in arb_period()) {
        let doc = p.doc();
        let Some(h) = Hypnogram::from_doc(&doc) else { return Ok(()) };
        let codes: Vec<char> = p.codes().chars().collect();
        let at = |i: usize| p.start + Duration::seconds(i as i64 * 30);
        let first_asleep = codes.iter().position(|c| *c != '4');
        let last_asleep = codes.iter().rposition(|c| *c != '4');
        let latency = p.latency_epochs.filter(|l| *l <= codes.len());
        let e = events(&h, p.latency_epochs.map(|l| (l * 30) as f64));
        prop_assert_eq!(e.first_sleep_epoch, first_asleep.map(at));
        prop_assert_eq!(e.woke_for_good, last_asleep.map(|i| at(i + 1)));
        prop_assert_eq!(e.first_deep, codes.iter().position(|c| *c == '1').map(at));
        prop_assert_eq!(e.first_rem, codes.iter().position(|c| *c == '3').map(at));
        prop_assert_eq!(e.fell_asleep, latency.map(at).or(first_asleep.map(at)));
        let (wake, long_wakes) = match (first_asleep, last_asleep) {
            (Some(a), Some(b)) => {
                let onset = latency.unwrap_or(a);
                // Awake runs clipped to [onset, b].
                let mut runs: Vec<usize> = Vec::new();
                let mut current = 0usize;
                for (i, c) in codes.iter().enumerate() {
                    if *c == '4' && i >= onset && i <= b {
                        current += 1;
                    } else if current > 0 {
                        runs.push(current);
                        current = 0;
                    }
                }
                if current > 0 {
                    runs.push(current);
                }
                (runs.iter().sum::<usize>() as i64 * 30, runs.iter().filter(|n| **n >= 10).count())
            }
            _ => (0, 0),
        };
        prop_assert_eq!(e.awake_between_seconds, wake);
        prop_assert_eq!(e.awakenings_5min, long_wakes);
        let m = movement(&doc).unwrap();
        let count = |code: char| p.movement.iter().filter(|c| **c == code).count() as i64 * 30;
        prop_assert_eq!(m, vec![("still", count('1')), ("restless", count('2')), ("tossing and turning", count('3')), ("active", count('4'))]);
    }

    /// The JSON segments tile the period: contiguous, no gaps or overlaps,
    /// right stage labels, summing to the time in bed.
    #[test]
    fn stage_segments_tile_the_period(p in arb_period()) {
        let v = period_json(&p.doc());
        let Some(segs) = v["segments"].as_array() else {
            prop_assert!(p.runs.is_empty());
            return Ok(());
        };
        let model = merged(&p.runs);
        prop_assert_eq!(segs.len(), model.len());
        let mut cursor = p.start;
        for (s, (code, n)) in segs.iter().zip(&model) {
            let (start, end) = (ts(s["start"].as_str().unwrap()), ts(s["end"].as_str().unwrap()));
            prop_assert_eq!(start, cursor);
            prop_assert_eq!(s["seconds"].as_i64().unwrap(), *n as i64 * 30);
            prop_assert_eq!((end - start).num_seconds(), *n as i64 * 30);
            let label = ["deep", "light", "rem", "awake"][(*code as u8 - b'1') as usize];
            prop_assert_eq!(s["stage"].as_str(), Some(label));
            cursor = end;
        }
        prop_assert_eq!((cursor - p.start).num_seconds(), p.epochs() as i64 * 30);
    }

    /// Every chart column marks exactly one stage, the most common in its
    /// bin; at most `width` columns; hour labels name the hour that starts at
    /// their column and never overlap; the scale line is right.
    #[test]
    fn stage_chart_is_faithful(p in arb_period(), width in 20usize..120) {
        let Some(h) = Hypnogram::from_doc(&p.doc()) else { return Ok(()) };
        let chart = render_chart(&h, width);
        let lines: Vec<&str> = chart.lines().collect();
        let rows: Vec<Vec<char>> = lines[..4].iter().map(|l| l.chars().skip(8).collect()).collect();
        let per = h.epochs.len().div_ceil(width);
        let bins: Vec<&[Option<Stage>]> = h.epochs.chunks(per).collect();
        prop_assert!(rows.iter().all(|r| r.len() <= width && r.len() <= bins.len()), "a row is too wide");
        for (col, bin) in bins.iter().enumerate() {
            let marked: Vec<Stage> = [Stage::Awake, Stage::Rem, Stage::Light, Stage::Deep]
                .into_iter()
                .zip(&rows)
                .filter(|(_, r)| r.get(col) == Some(&'█'))
                .map(|(s, _)| s)
                .collect();
            prop_assert_eq!(marked.len(), 1, "column {} marks {:?}", col, marked);
            let count = |s: Stage| bin.iter().filter(|x| **x == Some(s)).count();
            let best = Stage::ALL.into_iter().map(count).max().unwrap();
            prop_assert_eq!(count(marked[0]), best, "column {} is not the bin's most common stage", col);
        }
        // Hour labels, read back from the axis line.
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
        prop_assert_eq!(labels.first().map(|(c, t)| (*c, t.clone())), Some((0, h.start.format("%H:%M").to_string())));
        for (col, text) in labels.iter().skip(1) {
            let here = h.at(col * per);
            prop_assert!(*col < bins.len(), "label past the chart at {}", col);
            prop_assert_eq!(text.clone(), here.format("%H:00").to_string(), "label at column {}", col);
            prop_assert!(h.at((col - 1) * per).hour() != here.hour(), "label at {} is not an hour boundary", col);
        }
        // Completeness: every hour boundary that has room after the previous
        // label (one space, and the label fits) gets a label.
        let mut room_from = labels[0].1.len() + 1;
        let mut want = vec![0usize];
        for col in 1..bins.len() {
            let boundary = h.at((col - 1) * per).hour() != h.at(col * per).hour();
            if boundary && col >= room_from && col + 5 <= bins.len() + 6 {
                want.push(col);
                room_from = col + 6;
            }
        }
        prop_assert_eq!(labels.iter().map(|(c, _)| *c).collect::<Vec<_>>(), want, "hour labels missing or extra");
        prop_assert_eq!(lines[5].trim(), format!("one column ≈ {}", fmt_duration((per as i64 * h.epoch_secs) as f64).unwrap()));
    }

    /// Bins with no known stage (unknown codes only) mark nothing; others mark
    /// their most common known stage.
    #[test]
    fn stage_chart_with_unknown_codes(codes in "[1-4xx]{1,300}", width in 20usize..80) {
        let h = Hypnogram::from_doc(&json!({"bedtime_start": "2026-10-18T00:00:00Z", "sleep_phase_30_sec": codes})).unwrap();
        let chart = render_chart(&h, width);
        let rows: Vec<Vec<char>> = chart.lines().take(4).map(|l| l.chars().skip(8).collect()).collect();
        let per = h.epochs.len().div_ceil(width);
        for (col, bin) in h.epochs.chunks(per).enumerate() {
            let marks = rows.iter().filter(|r| r.get(col) == Some(&'█')).count();
            let known = bin.iter().filter(|x| x.is_some()).count();
            prop_assert_eq!(marks, usize::from(known > 0), "column {} of {:?}", col, bin);
        }
    }

    /// The nightly summary picks, per day, the longest confirmed long_sleep
    /// (else the longest other confirmed period), copies its fields without
    /// inventing any, and counts the other confirmed periods. Deleted and
    /// rejected (`rest`) periods never count.
    #[test]
    fn oura_nights_match_the_model(periods in arb_periods()) {
        let docs: Vec<Value> = periods.iter().map(Period::doc).collect();
        let days: Vec<NaiveDate> = periods.iter().map(|p| p.day).collect::<BTreeSet<_>>().into_iter().collect();
        let nights = oura::summarize(&docs, &[], &[], &days);
        for day in &days {
            let live: Vec<&Period> = periods.iter().filter(|p| p.day == *day && confirmed(p.kind)).collect();
            let longs: Vec<&Period> = live.iter().copied().filter(|p| p.kind == "long_sleep").collect();
            let pool = if longs.is_empty() { &live } else { &longs };
            let Some(best) = pool.iter().map(|p| p.asleep_secs()).max() else {
                prop_assert!(!nights.contains_key(day), "{} has no confirmed periods", day);
                continue;
            };
            let n = &nights[day];
            let chosen = live.iter().find(|p| Some(p.id.as_str()) == n.source_id.as_deref()).expect("source is a confirmed period of the day");
            prop_assert!(pool.iter().any(|p| p.id == chosen.id), "picked from the wrong pool");
            // Ties go to the earliest bedtime, then the smallest id.
            let first = pool.iter().filter(|p| p.asleep_secs() == best).min_by_key(|p| (p.start, p.id.clone())).unwrap();
            prop_assert_eq!(&chosen.id, &first.id, "tie broken wrongly");
            prop_assert_eq!(n.other_periods, live.len() - 1);
            prop_assert_eq!(n.asleep_s, Some(best as f64));
            prop_assert_eq!(n.time_in_bed_s, Some(chosen.epochs() as f64 * 30.0));
            prop_assert_eq!(n.hrv_avg_ms, chosen.metrics[0]);
            prop_assert_eq!(n.hr_lowest_bpm, chosen.metrics[1]);
            prop_assert_eq!(n.latency_s, chosen.latency_epochs.map(|l| l as f64 * 30.0));
        }
        prop_assert!(nights.keys().all(|d| days.contains(d)));
    }

    /// A period with no total ranks below one with any total, including 0.
    #[test]
    fn missing_totals_rank_last(total in 0u32..10, null_first in any::<bool>()) {
        let a = json!({"id": "with", "type": "long_sleep", "day": "2026-10-18", "total_sleep_duration": total});
        let b = json!({"id": "without", "type": "long_sleep", "day": "2026-10-18", "total_sleep_duration": null});
        let pick = if null_first { oura::main_period(&[&b, &a]) } else { oura::main_period(&[&a, &b]) };
        prop_assert_eq!(pick.and_then(|p| p["id"].as_str()), Some("with"));
    }

    /// Each Oura field lands in its own Night field (distinct sentinels catch
    /// swaps); a null field stays unknown; score-only days keep their scores.
    #[test]
    fn oura_fields_map_one_to_one(x in 1u32..100, score_only in any::<bool>()) {
        let day = d("2026-10-18");
        let pairs = [
            ("time_in_bed", "time_in_bed_s"), ("total_sleep_duration", "asleep_s"), ("deep_sleep_duration", "deep_s"),
            ("rem_sleep_duration", "rem_s"), ("light_sleep_duration", "light_s"), ("awake_time", "awake_s"),
            ("latency", "latency_s"), ("efficiency", "efficiency_pct"), ("average_heart_rate", "hr_avg_bpm"),
            ("lowest_heart_rate", "hr_lowest_bpm"), ("average_hrv", "hrv_avg_ms"), ("average_breath", "breath_avg_per_min"),
            ("restless_periods", "restless_periods"),
        ];
        let mut p = json!({"day": day.to_string(), "type": "long_sleep", "id": "main"});
        for (i, (input, _)) in pairs.iter().enumerate() {
            p[*input] = json!(x + i as u32);
        }
        let sleep = if score_only { vec![] } else { vec![p.clone()] };
        let ds = json!({"day": day.to_string(), "score": 81});
        let ready = json!({"day": day.to_string(), "score": 72, "temperature_deviation": -0.25});
        let got = oura::summarize(&sleep, &[ds], &[ready], &[day]);
        let n = &got[&day];
        prop_assert_eq!((n.sleep_score, n.readiness_score, n.temp_deviation_c), (Some(81.0), Some(72.0), Some(-0.25)));
        let v = serde_json::to_value(n).unwrap();
        for (i, (input, output)) in pairs.iter().enumerate() {
            prop_assert_eq!(v[*output].as_f64(), if score_only { None } else { Some((x + i as u32) as f64) }, "{} -> {}", input, output);
            let mut missing = p.clone();
            missing[*input] = Value::Null;
            let out = oura::summarize(&[missing], &[], &[], &[day]);
            prop_assert!(serde_json::to_value(&out[&day]).unwrap()[*output].is_null(), "null {} became a value", input);
        }
    }

    /// Range lookup against an independent membership rule, per collection:
    /// a day document is in when its day is; a span when it overlaps; a
    /// rest-mode period with no end is ongoing; an enhanced tag with no end
    /// is a point on its start day. Duplicates are kept; reversed ranges are empty.
    #[test]
    fn range_lookup_matches_constructed_intervals(
        collection in prop::sample::select(vec!["sleep", "rest_mode_period", "enhanced_tag"]),
        rows in prop::collection::vec((-60i64..60, prop::option::of(0i64..60), 0u8..4), 0..40),
        lo in -30i64..30,
        len in -3i64..30,
    ) {
        let base = d("2026-10-18");
        let (start, end) = (base + Duration::days(lo), base + Duration::days(lo + len));
        let mut docs = Vec::new();
        let mut expected = Vec::new();
        for (a, length, id) in rows {
            let first = base + Duration::days(a);
            let doc = if collection == "sleep" {
                json!({"id": id, "day": first.to_string()})
            } else {
                json!({"id": id, "start_day": first.to_string(), "end_day": length.map(|n| (first + Duration::days(n)).to_string())})
            };
            let last = match (collection, length) {
                ("sleep", _) => first,
                (_, Some(n)) => first + Duration::days(n),
                ("enhanced_tag", None) => first,
                (_, None) => NaiveDate::MAX,
            };
            if start <= end && first <= end && last >= start {
                expected.push(doc.to_string());
            }
            docs.push(doc);
        }
        let mut got: Vec<String> = views::select(collection, docs, start, end).iter().map(Value::to_string).collect();
        got.sort();
        expected.sort();
        prop_assert_eq!(got, expected);
    }

    /// Pages are concatenated in order whatever the page sizes, including
    /// empty pages in the middle; tokens are passed back exactly.
    #[test]
    fn pagination_reassembles_any_split(pages in prop::collection::vec(prop::collection::vec(0u8..4, 0..5), 1..6), token in "[A-Za-z0-9+/=%_-]{1,20}") {
        let tokens: Vec<Option<String>> = (0..pages.len()).map(|i| (i > 0).then(|| format!("{i}{token}"))).collect();
        let mut calls = 0;
        let got = oura::paginate(|t| {
            anyhow::ensure!(calls < pages.len(), "asked for more pages than exist");
            anyhow::ensure!(t == tokens[calls].as_deref(), "token {:?} passed back as {:?}", tokens[calls], t);
            let page = json!({"data": pages[calls], "next_token": tokens.get(calls + 1).cloned().flatten()});
            calls += 1;
            Ok(page)
        });
        let got = got.map_err(|e| TestCaseError::fail(e.to_string()))?;
        prop_assert_eq!(calls, pages.len());
        prop_assert_eq!(got, pages.concat().into_iter().map(Value::from).collect::<Vec<_>>());
    }

    /// An empty or repeated token is an error, never a loop; a fetch error
    /// comes back unchanged.
    #[test]
    fn pagination_fails_safely(n in 1usize..8, fail_at in 0usize..8) {
        let mut calls = 0;
        let cycle = oura::paginate(|_| {
            calls += 1;
            anyhow::ensure!(calls <= n + 2, "test guard: looping");
            Ok(json!({"data": [], "next_token": format!("p{}", (calls - 1) % n)}))
        });
        prop_assert!(cycle.unwrap_err().to_string().contains("repeated"));
        prop_assert_eq!(calls, n + 1);
        let mut calls = 0;
        let empty = oura::paginate(|_| {
            calls += 1;
            Ok(json!({"data": [1], "next_token": ""}))
        });
        prop_assert!(empty.is_err() && calls == 1, "an empty token was followed ({} calls)", calls);
        let mut calls = 0;
        let error = oura::paginate(|_| {
            anyhow::ensure!(calls != fail_at, "sentinel fetch error");
            calls += 1;
            Ok(json!({"data": [calls], "next_token": calls.to_string()}))
        });
        prop_assert_eq!(error.unwrap_err().to_string(), "sentinel fetch error");
    }

    /// Heart-rate buckets equal a direct count per local day, or per local
    /// clock hour identified by its start instant (so the two 01:00 hours of
    /// a DST fall-back stay apart). No numeric sample is lost or doubled.
    #[test]
    fn heart_rate_buckets_match_the_model(samples in prop::collection::vec(arb_hr_sample(), 0..80), by_hour in any::<bool>()) {
        let got = views::hr_buckets(&samples, ny(), by_hour);
        type Bucket = (usize, f64, f64, f64, BTreeMap<String, usize>); // count, min, max, sum, sources
        let mut model: BTreeMap<views::HrKey, Bucket> = BTreeMap::new();
        for s in &samples {
            let Some(bpm) = s["bpm"].as_f64() else { continue };
            let t = ts(s["timestamp"].as_str().unwrap()).with_timezone(&ny());
            // New York offsets are whole hours, so the hour starts at the
            // timestamp minus its minutes and seconds.
            let hour = by_hour.then(|| (t - Duration::seconds(i64::from(t.minute() * 60 + t.second())) - Duration::nanoseconds(i64::from(t.nanosecond()))).fixed_offset());
            let e = model.entry((t.date_naive(), hour)).or_insert((0, f64::INFINITY, f64::NEG_INFINITY, 0.0, BTreeMap::new()));
            e.0 += 1;
            e.1 = e.1.min(bpm);
            e.2 = e.2.max(bpm);
            e.3 += bpm;
            *e.4.entry(s["source"].as_str().unwrap().to_string()).or_default() += 1;
        }
        prop_assert_eq!(got.len(), model.len());
        for (key, (count, min, max, sum, sources)) in model {
            let s = &got[&key];
            prop_assert_eq!((s.samples, s.min, s.max), (count, min, max));
            prop_assert!((s.avg().unwrap() - sum / count as f64).abs() < 1e-9);
            prop_assert_eq!(&s.by_source, &sources);
        }
    }

    /// Eight Sleep nights against a model of a real-shaped response: durations
    /// from the main session's stageSummary (awake and out of bed apart),
    /// tonight's `current` values (never the baseline `average`), lowest heart
    /// rate and temperatures from the session's samples, day-level fallbacks
    /// when there is no session, and "None" as unknown.
    #[test]
    fn eight_nights_match_the_model(
        deep in 0u32..5000, rem in 0u32..5000, light in 0u32..15000, awake in 0u32..4000, out in 0u32..3000,
        current in 40u32..90, baseline in 40u32..90,
        heart in prop::collection::vec(prop::option::weighted(0.9, 40u32..90), 0..12),
        bed in prop::collection::vec(25u32..35, 0..6),
        nap_first in any::<bool>(), with_session in any::<bool>(),
    ) {
        let day = d("2026-10-18");
        let sleep = deep + rem + light;
        let total = sleep + awake + out;
        let main = json!({
            "id": "main",
            "stageSummary": {"totalDuration": total, "sleepDuration": sleep, "deepDuration": deep, "remDuration": rem,
                             "lightDuration": light, "awakeDuration": awake, "outDuration": out, "awakeBeforeSleepDuration": awake / 2},
            "timeseries": {
                "heartRate": heart.iter().enumerate().map(|(i, v)| json!([format!("2026-10-18T0{}:00:00.000Z", i % 10), v.map(Value::from).unwrap_or(json!("None"))])).collect::<Vec<_>>(),
                "tempBedC": bed.iter().map(|v| json!(["2026-10-18T03:00:00.000Z", v])).collect::<Vec<_>>(),
            },
        });
        let nap = json!({"id": "nap", "stageSummary": {"sleepDuration": 600, "awakeDuration": 1, "outDuration": 2, "totalDuration": 603}, "timeseries": {"heartRate": [["2026-10-18T19:00:00.000Z", 1]]}});
        let mut doc = json!({
            "day": day.to_string(), "presenceDuration": total + 7, "sleepDuration": sleep + 7, "deepDuration": deep + 7,
            "sleepQualityScore": {
                "heartRate": {"current": current, "average": baseline},
                "hrv": {"current": current + 1, "average": baseline + 1},
                "respiratoryRate": {"current": current + 2, "average": baseline + 2},
            },
        });
        if with_session {
            doc["mainSessionId"] = json!("main");
            doc["sessions"] = if nap_first { json!([nap, main]) } else { json!([main, nap]) };
        }
        let n = eight::summarize(&json!({"days": [doc]}), &[day]).unwrap().remove(&day).unwrap();
        prop_assert_eq!((n.hr_avg_bpm, n.hrv_avg_ms, n.breath_avg_per_min), (Some(current as f64), Some((current + 1) as f64), Some((current + 2) as f64)));
        if with_session {
            prop_assert_eq!(n.source_id.as_deref(), Some("main"));
            prop_assert_eq!(
                (n.asleep_s, n.deep_s, n.rem_s, n.light_s, n.awake_s, n.out_of_bed_s, n.time_in_bed_s, n.latency_s),
                (Some(sleep as f64), Some(deep as f64), Some(rem as f64), Some(light as f64), Some(awake as f64), Some(out as f64), Some(total as f64), Some((awake / 2) as f64))
            );
            prop_assert_eq!(n.hr_lowest_bpm, heart.iter().flatten().min().map(|v| *v as f64));
            let want_bed = (!bed.is_empty()).then(|| bed.iter().map(|v| *v as f64).sum::<f64>() / bed.len() as f64);
            prop_assert_eq!(n.bed_temp_c, want_bed);
        } else {
            // Day-level fallbacks; without a stageSummary awake and out of bed are unknown.
            prop_assert_eq!((n.asleep_s, n.deep_s, n.time_in_bed_s), (Some((sleep + 7) as f64), Some((deep + 7) as f64), Some((total + 7) as f64)));
            prop_assert_eq!((n.awake_s, n.out_of_bed_s, n.hr_lowest_bpm, n.bed_temp_c), (None, None, None, None));
        }
        // "None" for a current value is unknown, not the baseline.
        let mut none = json!({"days": [{"day": day.to_string(), "sleepQualityScore": {"hrv": {"current": "None", "average": baseline}}}]});
        let n = eight::summarize(&none, &[day]).unwrap().remove(&day).unwrap();
        prop_assert_eq!(n.hrv_avg_ms, None);
        none["days"][0]["hrv"] = json!(current);
        let n = eight::summarize(&none, &[day]).unwrap().remove(&day).unwrap();
        prop_assert_eq!(n.hrv_avg_ms, Some(current as f64), "top-level fallback");
    }

    /// Duplicate Eight days: the longer sleep wins whatever the order; score
    /// and latency fall back in the documented order.
    #[test]
    fn eight_duplicate_days_and_fallbacks(x in 1u32..90, swap in any::<bool>()) {
        let day = d("2026-10-18");
        let mut winner = json!({
            "day": day.to_string(), "sleepDuration": 20000, "presenceDuration": 25000, "score": x,
            "sleepFitnessScore": {"total": x + 1}, "latencyAsleepSeconds": 600,
            "sleepRoutineScore": {"latencyAsleepSeconds": {"current": 900}},
        });
        let mut loser = winner.clone();
        loser["sleepDuration"] = json!(19000);
        loser["presenceDuration"] = json!(30000);
        loser["score"] = json!(0);
        let run = |w: Value| {
            let days = if swap { json!([w, loser.clone()]) } else { json!([loser.clone(), w]) };
            eight::summarize(&json!({"days": days}), &[day]).unwrap().remove(&day).unwrap()
        };
        let n = run(winner.clone());
        prop_assert_eq!((n.sleep_score, n.latency_s), (Some(x as f64), Some(600.0)));
        // A known duration, even 0, beats an unknown one.
        for (field, unknown) in [("sleepDuration", Value::Null), ("presenceDuration", json!("None"))] {
            let mut known = json!({"day": day.to_string(), "sleepDuration": 0, "presenceDuration": 0, "score": 1});
            let mut blank = known.clone();
            blank[field] = unknown;
            blank["score"] = json!(2);
            if field == "presenceDuration" {
                known["presenceDuration"] = json!(0);
            }
            let days = if swap { json!([known, blank]) } else { json!([blank, known]) };
            let n = eight::summarize(&json!({"days": days}), &[day]).unwrap().remove(&day).unwrap();
            prop_assert_eq!(n.sleep_score, Some(1.0), "unknown {} won over a known 0", field);
        }
        winner["score"] = Value::Null;
        winner["latencyAsleepSeconds"] = json!("None");
        let n = run(winner);
        prop_assert_eq!((n.sleep_score, n.latency_s), (Some((x + 1) as f64), Some(900.0)));
    }

    /// Eight Sleep session expiry: a positive `expires_in` is used as given;
    /// zero, negative or missing means one hour.
    #[test]
    fn token_expiry_is_bounded(expires in prop_oneof![Just(None), (-5i64..5).prop_map(Some), (1i64..200_000).prop_map(Some), any::<i64>().prop_map(Some)], huge in prop_oneof![Just(f64::MAX), 1e15f64..1e300], now in 0i64..2_000_000_000) {
        // Eight Sleep: a positive expiry is used as given (capped at ten
        // years); zero, negative or missing means one hour.
        let mut v = json!({"access_token": "t", "userId": "u1"});
        if let Some(e) = expires {
            v["expires_in"] = json!(e);
        }
        let s = eight::session_from_response(&v, "a@b.c", now).unwrap();
        let ten_years = 10 * 365 * 86400;
        let want = match expires {
            Some(e) if e > 0 => now + e.min(ten_years),
            _ => now + 3600,
        };
        prop_assert_eq!(s.expires_at, want);
        // Absurd expiries from either provider never overflow.
        let s = eight::session_from_response(&json!({"access_token": "t", "userId": "u1", "expires_in": huge}), "a@b.c", now).unwrap();
        prop_assert_eq!(s.expires_at, now + ten_years);
        let t = oura::tokens_from_response(&json!({"access_token": "a", "refresh_token": "r", "expires_in": huge}), now, None).unwrap();
        prop_assert_eq!(t.expires_at, now + ten_years);
    }
}

// ======================================================================
// 3. Metamorphic properties
// ======================================================================

proptest! {
    /// Oura returns documents in its own order; the nightly pick must not
    /// depend on it (ties included).
    #[test]
    fn oura_nights_ignore_input_order((periods, shuffled) in arb_periods().prop_flat_map(|p| (Just(p.clone()), Just(p).prop_shuffle()))) {
        let days: Vec<NaiveDate> = periods.iter().map(|p| p.day).collect::<BTreeSet<_>>().into_iter().collect();
        let a = oura::summarize(&periods.iter().map(Period::doc).collect::<Vec<_>>(), &[], &[], &days);
        let b = oura::summarize(&shuffled.iter().map(Period::doc).collect::<Vec<_>>(), &[], &[], &days);
        prop_assert_eq!(a, b);
    }

    /// Same for Eight Sleep's trends days.
    #[test]
    fn eight_nights_ignore_input_order((days, shuffled) in prop::collection::vec(arb_eight_day(), 0..8).prop_flat_map(|d| (Just(d.clone()), Just(d).prop_shuffle()))) {
        let wanted: Vec<NaiveDate> = days.iter().filter_map(|x| x["day"].as_str()).map(d).collect();
        let a = eight::summarize(&json!({"days": days}), &wanted).unwrap();
        let b = eight::summarize(&json!({"days": shuffled}), &wanted).unwrap();
        prop_assert_eq!(a.len(), wanted.iter().collect::<BTreeSet<_>>().len(), "every requested day with an entry is summarized");
        prop_assert_eq!(a, b);
    }

    /// Looking up a range equals looking up its two halves.
    #[test]
    fn range_lookup_splits(collection in prop::sample::select(vec!["sleep", "rest_mode_period", "enhanced_tag"]), raw in prop::collection::vec(arb_collection_doc("rest_mode_period"), 0..30), a in arb_day(), len in 0i64..12, cut in 0i64..12) {
        let docs: Vec<Value> = if collection == "sleep" { raw.into_iter().map(|x| json!({"id": x["id"], "day": x["start_day"]})).collect() } else { raw };
        let (start, end) = (a, a + Duration::days(len));
        let mid = start + Duration::days(cut.min(len));
        let set = |s: NaiveDate, e: NaiveDate| views::select(collection, docs.clone(), s, e).iter().map(Value::to_string).collect::<BTreeSet<_>>();
        let mut halves = set(start, mid);
        if mid < end {
            halves.extend(set(mid + Duration::days(1), end));
        }
        prop_assert_eq!(set(start, end), halves);
    }

    /// The query window makes the result independent of whether Oura's date
    /// bounds are inclusive or exclusive. Simulate each server rule (filter
    /// on the day, or the start day for spans) and require the same
    /// documents as filtering everything locally.
    #[test]
    fn range_lookup_is_independent_of_server_bounds(collection in prop::sample::select(vec!["sleep", "rest_mode_period", "enhanced_tag"]), raw in prop::collection::vec(arb_collection_doc("rest_mode_period"), 0..30), a in arb_day(), len in 0i64..12, rule in 0u8..4) {
        let docs: Vec<Value> = if collection == "sleep" { raw.into_iter().map(|x| json!({"id": x["id"], "day": x["start_day"]})).collect() } else { raw };
        let (start, end) = (a, a + Duration::days(len));
        let (from, to) = oura::query_window(collection, start, end);
        let served: Vec<Value> = docs.iter().filter(|doc| {
            let day = views::doc_day(doc).unwrap();
            let lo = if rule & 1 == 0 { day >= from } else { day > from };
            let hi = if rule & 2 == 0 { day <= to } else { day < to };
            lo && hi
        }).cloned().collect();
        let render = |v: Vec<Value>| v.iter().map(Value::to_string).collect::<Vec<_>>();
        prop_assert_eq!(render(views::select(collection, served, start, end)), render(views::select(collection, docs, start, end)));
    }

    /// The same instants written in another UTC offset give the same stage
    /// totals and the same event instants.
    #[test]
    fn stages_ignore_how_the_start_is_written(p in arb_period(), other in arb_offset()) {
        let mut moved = p.clone();
        moved.start = p.start.with_timezone(&other);
        let (Some(h1), Some(h2)) = (Hypnogram::from_doc(&p.doc()), Hypnogram::from_doc(&moved.doc())) else {
            prop_assert!(p.runs.is_empty());
            return Ok(());
        };
        prop_assert_eq!(stage_stats(&h1), stage_stats(&h2));
        let lat = p.latency_epochs.map(|l| (l * 30) as f64);
        prop_assert_eq!(events(&h1, lat), events(&h2, lat));
    }

    /// The same stages at 5-minute resolution (each code ten times as 30-second
    /// epochs) give the same totals, events and segments; the 30-second
    /// string wins when both exist; missing, empty or "None" strings fall back.
    #[test]
    fn stage_resolutions_agree(codes in "[1-4x]{1,50}") {
        let start = "2026-10-18T00:00:00Z";
        let coarse = json!({"bedtime_start": start, "sleep_phase_5_min": codes});
        let fine = json!({"bedtime_start": start, "sleep_phase_30_sec": codes.chars().flat_map(|c| std::iter::repeat_n(c, 10)).collect::<String>()});
        let (a, b) = (Hypnogram::from_doc(&coarse).unwrap(), Hypnogram::from_doc(&fine).unwrap());
        prop_assert_eq!((a.epoch_secs, b.epoch_secs), (300, 30));
        prop_assert_eq!(stage_stats(&a), stage_stats(&b));
        prop_assert_eq!(events(&a, None), events(&b, None));
        prop_assert_eq!(&period_json(&coarse)["segments"], &period_json(&fine)["segments"]);
        for absent in [Value::Null, json!(""), json!("None")] {
            let mut doc = coarse.clone();
            doc["sleep_phase_30_sec"] = absent;
            prop_assert_eq!(Hypnogram::from_doc(&doc), Some(a.clone()));
        }
        let app = json!({"bedtime_start": start, "app_sleep_phase_5_min": codes});
        prop_assert_eq!(Hypnogram::from_doc(&app), Some(a));
        let mut both = fine;
        both["sleep_phase_5_min"] = json!("444");
        both["app_sleep_phase_5_min"] = json!("333");
        prop_assert_eq!(Hypnogram::from_doc(&both), Some(b));
    }

    /// Unknown codes keep their time as a gap: they split runs, count in no
    /// stage, and don't move what comes after.
    #[test]
    fn unknown_epochs_keep_time(n in 1usize..30, gap in 1usize..30) {
        let doc = json!({"bedtime_start": "2026-10-18T00:00:00Z", "sleep_phase_30_sec": format!("{}{}{}", "1".repeat(n), "?".repeat(gap), "1".repeat(n))});
        let h = Hypnogram::from_doc(&doc).unwrap();
        let stats = stage_stats(&h);
        prop_assert_eq!(&stats[0].0, &Stage::Deep);
        prop_assert_eq!((stats[0].1.seconds, stats[0].1.longest_seconds, stats[0].1.stretches_5min), (60 * n as i64, 30 * n as i64, if n >= 10 { 2 } else { 0 }));
        let segs = period_json(&doc)["segments"].as_array().unwrap().clone();
        prop_assert_eq!(segs.len(), 3);
        prop_assert!(segs[1]["stage"].is_null());
        prop_assert_eq!(segs[1]["seconds"].as_i64(), Some(30 * gap as i64));
        prop_assert_eq!(ts(segs[2]["start"].as_str().unwrap()), h.start + Duration::seconds(30 * (n + gap) as i64));
        prop_assert_eq!(events(&h, None).woke_for_good, Some(h.start + Duration::seconds(30 * (2 * n + gap) as i64)));
    }

    /// Heart-rate samples rewritten in another offset land in the same buckets.
    #[test]
    fn heart_rate_buckets_ignore_how_times_are_written(samples in prop::collection::vec(arb_hr_sample(), 0..40), other in arb_offset(), by_hour in any::<bool>()) {
        let moved: Vec<Value> = samples.iter().map(|s| {
            let mut s = s.clone();
            s["timestamp"] = json!(ts(s["timestamp"].as_str().unwrap()).with_timezone(&other).to_rfc3339());
            s
        }).collect();
        prop_assert_eq!(views::hr_buckets(&samples, ny(), by_hour), views::hr_buckets(&moved, ny(), by_hour));
    }
}

// ======================================================================
// 4. Security postconditions
// ======================================================================

const SENSITIVE: [&str; 9] = ["code", "state", "client_id", "client_secret", "access_token", "refresh_token", "password", "next_token", "code_challenge"];

proptest! {
    /// No value of a sensitive parameter reaches the log, raw or
    /// form-encoded, nor any URL userinfo; every other parameter is kept
    /// exactly; redacting twice changes nothing.
    #[test]
    fn log_urls_never_contain_secrets(
        secrets in prop::collection::vec((prop::sample::select(SENSITIVE.to_vec()), "SeCrEt[A-Za-z0-9+/=&%#?~ ]{4,16}"), 1..4),
        plain in prop::collection::vec((prop::sample::select(vec!["start_date", "end_date", "scope", "redirect_uri", "tz"]), "[A-Za-z0-9:/ +-]{0,12}"), 0..4),
        userinfo in prop::option::of("UsEr[A-Za-z0-9]{4,10}"),
    ) {
        let mut pairs: Vec<(String, String)> = secrets.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        pairs.extend(plain.iter().map(|(k, v)| (k.to_string(), v.clone())));
        let mut url = reqwest::Url::parse_with_params("https://api.ouraring.com/v2/usercollection/sleep", &pairs).unwrap();
        if let Some(u) = &userinfo {
            url.set_username(u).unwrap();
            url.set_password(Some(u)).unwrap();
        }
        let logged = log::redact_url(&url);
        for (_, secret) in &secrets {
            let form: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
            prop_assert!(!logged.contains(secret.as_str()) && !logged.contains(&form) && !logged.contains("SeCrEt"), "{} leaked in {}", secret, logged);
        }
        prop_assert!(!logged.contains("UsEr"), "userinfo leaked in {}", logged);
        let reparsed = reqwest::Url::parse(&logged).unwrap();
        let kept: Vec<(String, String)> = reparsed.query_pairs().into_owned().filter(|(k, _)| !SENSITIVE.contains(&k.as_str())).collect();
        prop_assert_eq!(kept, plain.iter().map(|(k, v)| (k.to_string(), v.clone())).collect::<Vec<_>>());
        prop_assert_eq!(log::redact_url(&reparsed), logged);
    }

    /// Any non-empty code survives the redirect round trip, through the
    /// listener or a paste; a different state is never accepted; an empty
    /// code is never accepted.
    #[test]
    fn oauth_redirect_round_trips_and_checks_state(code in "\\PC{1,40}", state in "\\PC{1,40}", other in "\\PC{1,40}") {
        let url = reqwest::Url::parse_with_params("http://localhost:8765/callback", &[("code", code.as_str()), ("state", state.as_str())]).unwrap();
        let target = format!("{}?{}", url.path(), url.query().unwrap());
        prop_assert_eq!(oura::parse_callback(&target, &state), oura::Callback::Code(code.clone()));
        prop_assert_eq!(oura::parse_pasted(url.as_str(), &state), oura::Callback::Code(code.clone()));
        prop_assert_eq!(oura::parse_pasted(url.as_str().trim_start_matches("http://"), &state), oura::Callback::Code(code.clone()));
        if other != state {
            prop_assert!(!matches!(oura::parse_callback(&target, &other), oura::Callback::Code(_)));
            prop_assert!(!matches!(oura::parse_pasted(url.as_str(), &other), oura::Callback::Code(_)));
        }
        let empty = format!("/callback?code=&state={}", url::form_urlencoded::byte_serialize(state.as_bytes()).collect::<String>());
        prop_assert!(!matches!(oura::parse_callback(&empty, &state), oura::Callback::Code(_)), "empty code accepted");
    }

    /// A state that differs in one or two places is rejected, including two
    /// differences that would cancel out in a broken constant-time compare.
    #[test]
    fn oauth_near_miss_states_are_rejected(state in "[a-f0-9]{8,64}", i in any::<prop::sample::Index>(), j in any::<prop::sample::Index>(), mask in 1u8..8) {
        let mut bytes = state.clone().into_bytes();
        let (i, j) = (i.index(bytes.len()), j.index(bytes.len()));
        bytes[i] ^= mask;
        if j != i {
            bytes[j] ^= mask;
        }
        let other = String::from_utf8(bytes).unwrap();
        prop_assert_ne!(&other, &state);
        let target = format!("/callback?code=abc&state={}", url::form_urlencoded::byte_serialize(other.as_bytes()).collect::<String>());
        prop_assert!(!matches!(oura::parse_callback(&target, &state), oura::Callback::Code(_)));
    }
}

// ======================================================================
// 5. Rendering and the dashboard
// ======================================================================

proptest! {
    /// Zero shows as 0m, unknown as —; a source with nothing says so.
    #[test]
    fn night_keeps_zero_and_unknown_apart(minutes in 0u32..60) {
        let day = d("2026-10-18");
        let known = Night { asleep_s: Some(minutes as f64 * 60.0), ..Night::default() };
        let out = night::render_night(day, ny(), &[
            (Source::Oura, Slot::Record { night: Box::new(known) }),
            (Source::EightSleep, Slot::Record { night: Box::new(Night::default()) }),
        ]);
        let cells: Vec<&str> = field(&out, "asleep").split_whitespace().collect();
        let want = format!("{minutes}m");
        prop_assert_eq!(cells, vec![want.as_str(), "—"]);
        let absent = night::render_night(day, ny(), &[
            (Source::Oura, Slot::NoRecord),
            (Source::EightSleep, Slot::Unavailable { reason: "sentinel".into() }),
        ]);
        prop_assert!(absent.contains("no record") && absent.contains("unavailable") && absent.contains("Eight Sleep: sentinel"));
        prop_assert!(!absent.lines().any(|l| l.starts_with("asleep")));
    }

    /// Oura times show the record's own clock and offset; Eight Sleep's are
    /// converted to the display zone.
    #[test]
    fn night_times_follow_each_source(minutes in 0i64..120) {
        let t = ts("2026-10-18T02:00:00+02:00") + Duration::minutes(minutes);
        let n = Night { bedtime_start: Some(t), bedtime_end: Some(t + Duration::hours(1)), ..Night::default() };
        let out = night::render_night(d("2026-10-18"), ny(), &[
            (Source::Oura, Slot::Record { night: Box::new(n.clone()) }),
            (Source::EightSleep, Slot::Record { night: Box::new(n) }),
        ]);
        let line = field(&out, "in bed");
        let own = format!("{} → {} (+02:00)", t.format("%H:%M"), (t + Duration::hours(1)).format("%H:%M"));
        let home = format!("{} → {}", t.with_timezone(&ny()).format("%H:%M"), (t + Duration::hours(1)).with_timezone(&ny()).format("%H:%M"));
        prop_assert!(line.starts_with(&own) && line.ends_with(&home), "{}", line);
    }

    /// Sample summaries count only present values; contributor scores are
    /// out of 100; an unknown future field still appears.
    #[test]
    fn detail_samples_count_only_present_values(n in 1usize..20, bpm in 40u32..100) {
        let items: Vec<Value> = (0..n).flat_map(|_| [json!(bpm), Value::Null]).collect();
        let out = views::render_detail(&[], &json!({
            "heart_rate": {"items": items, "interval": 300, "timestamp": "2026-10-18T00:00:00Z"},
            "contributors": {"total_sleep": 81},
            "future_field": null,
        }), ny());
        prop_assert!(out.contains(&format!("{n} samples every 300s · avg {bpm} bpm · min {bpm} · max {bpm}")), "{}", out);
        prop_assert!(out.contains("81 / 100"));
        prop_assert!(out.lines().any(|l| l.trim_start().starts_with("future field") && l.ends_with('—')));
    }

    /// The dashboard's sleep line shows the main confirmed period and counts
    /// naps, never deleted or rejected periods.
    #[test]
    fn dashboard_uses_the_main_confirmed_period(hours in 4u32..9) {
        let day = d("2026-10-18");
        let mk = |id: &str, kind: &str, h: u32| json!({"id": id, "day": day.to_string(), "type": kind, "total_sleep_duration": h * 3600});
        let main = mk("main", "long_sleep", hours);
        let docs = vec![mk("nap", "late_nap", hours + 1), mk("deleted", "deleted", hours + 2), mk("rejected", "rest", hours + 3), main.clone(), mk("short", "long_sleep", hours - 1)];
        let n = oura::summarize(&docs, &[], &[], &[day]);
        prop_assert_eq!(n[&day].source_id.as_deref(), Some("main"));
        prop_assert_eq!(n[&day].other_periods, 2);
        let out = views::render_day(day, ny(), &BTreeMap::from([("sleep".to_string(), docs)]), &BTreeMap::new());
        let line = field(&out, "sleep");
        prop_assert!(line.contains(&format!("{hours}h 00m asleep")) && line.contains("+2 other period(s)"), "{}", line);
        let mut same = main.clone();
        same["id"] = json!("aaa");
        prop_assert_eq!(oura::main_period(&[&main, &same]), Some(&same));
    }

    /// A section whose collection failed never claims "none" or "off".
    #[test]
    fn dashboard_never_calls_unavailable_empty(pair in prop::sample::select(vec![("workout", "workouts"), ("session", "sessions"), ("rest_mode_period", "rest mode"), ("tag", "tags"), ("enhanced_tag", "tags"), ("daily_stress", "stress"), ("sleep", "sleep")])) {
        let errors = BTreeMap::from([(pair.0.to_string(), "sentinel unavailable".to_string())]);
        let out = views::render_day(d("2026-10-18"), ny(), &BTreeMap::new(), &errors);
        let value = field(&out, pair.1);
        prop_assert!(value.contains("unavailable"), "misleading {} line: {}", pair.1, value);
        prop_assert!(out.contains("sentinel unavailable"));
    }

    /// Ties broken by bedtime use full precision: a millisecond earlier wins
    /// and sorts first.
    #[test]
    fn subsecond_ordering(ms in 0u32..999) {
        let a = ts("2026-10-18T01:00:00Z") + Duration::milliseconds(ms as i64);
        let early = json!({"id": "z", "day": "2026-10-18", "type": "long_sleep", "total_sleep_duration": 20000, "bedtime_start": a.to_rfc3339()});
        let late = json!({"id": "a", "day": "2026-10-18", "type": "long_sleep", "total_sleep_duration": 20000, "bedtime_start": (a + Duration::milliseconds(1)).to_rfc3339()});
        prop_assert_eq!(oura::main_period(&[&late, &early]), Some(&early));
        let mut sorted = vec![late, early.clone()];
        views::sort_docs(&mut sorted);
        prop_assert_eq!(&sorted[0], &early);
    }
}

// ======================================================================
// 6. Round trips, day arithmetic, and parsing boundaries
// ======================================================================

fn arb_date() -> impl Strategy<Value = NaiveDate> {
    (0i32..40_000).prop_map(|n| NaiveDate::from_num_days_from_ce_opt(720_000 + n).unwrap())
}

proptest! {
    #[test]
    fn day_specs_round_trip(day in arb_date(), today in arb_date(), n in prop_oneof![Just(0i64), Just(1), 0i64..=100_000]) {
        let spec: DaySpec = day.format("%Y-%m-%d").to_string().parse().unwrap();
        prop_assert_eq!(spec.resolve(today), day);
        let back: DaySpec = format!("-{n}").parse().unwrap();
        prop_assert_eq!((today - back.resolve(today)).num_days(), n);
        let shown: DaySpec = back.to_string().parse().unwrap();
        prop_assert_eq!(shown.resolve(today), back.resolve(today));
        for word in ["today", "", " Today ", "yesterday"] {
            let parsed: DaySpec = word.parse().unwrap();
            prop_assert_eq!(parsed.resolve(today), today - Duration::days(i64::from(word.trim().eq_ignore_ascii_case("yesterday"))));
        }
    }

    /// Anything that isn't a minus followed by digits within bounds is
    /// rejected, never misread or panicking.
    #[test]
    fn day_spec_rejects_malformed_offsets(n in prop_oneof![Just("1".to_string()), Just("9223372036854775808".to_string()), "[0-9]{1,25}"], prefix in prop::sample::select(vec!["--", "-+", "+-", "- "])) {
        let malformed = format!("{prefix}{n}");
        prop_assert!(malformed.parse::<DaySpec>().is_err(), "{} accepted", malformed);
        let back = format!("-{n}");
        match n.parse::<i64>() {
            Ok(v) if v <= 100_000 => prop_assert!(back.parse::<DaySpec>().is_ok()),
            _ => prop_assert!(back.parse::<DaySpec>().is_err()),
        }
    }

    #[test]
    fn days_inclusive_is_contiguous(start in arb_date(), len in 0i64..500) {
        let end = start + Duration::days(len);
        let days = days_inclusive(start, end);
        prop_assert_eq!(days.first(), Some(&start));
        prop_assert_eq!(days.last(), Some(&end));
        prop_assert_eq!(days.len() as i64, len + 1);
        prop_assert!(days.windows(2).all(|w| (w[1] - w[0]).num_days() == 1));
        prop_assert!(days_inclusive(end + Duration::days(1), end).is_empty());
    }

    /// A formatted duration reads back to within 30 seconds of the input,
    /// and longer inputs never format shorter. (Checked by reading the text
    /// back, not by repeating the formatter's arithmetic.)
    #[test]
    fn durations_read_back_close_and_ordered(a in prop_oneof![0.0f64..=7200.0, 0.0f64..=1e9, Just(29.999), Just(30.0), Just(3570.0), Just(3600.0)], gap in 0.0f64..=1e6) {
        let read = |s: String| -> Result<i64, TestCaseError> {
            let (h, m) = match s.split_once("h ") {
                Some((h, m)) => (h.parse::<i64>().unwrap(), m.trim_end_matches('m').parse::<i64>().unwrap()),
                None => (0, s.trim_end_matches('m').parse::<i64>().unwrap()),
            };
            prop_assert!(m < 60 || h == 0, "minutes past 59 with hours: {}", s);
            Ok(h * 60 + m)
        };
        let b = (a + gap).min(1e9);
        let (ma, mb) = (read(fmt_duration(a).unwrap())?, read(fmt_duration(b).unwrap())?);
        prop_assert!((ma as f64 * 60.0 - a).abs() <= 30.0, "{} s shown as {} min", a, ma);
        prop_assert!(ma <= mb);
        prop_assert_eq!(fmt_duration(-1.0 - gap), None);
        prop_assert_eq!(fmt_duration(1e9 + 1.0 + gap), None);
    }

    #[test]
    fn thousands_round_trips(n in -10_000_000_000i64..10_000_000_000i64) {
        let s = views::thousands(n as f64);
        prop_assert_eq!(s.replace(',', ""), n.to_string());
        let groups: Vec<&str> = s.trim_start_matches('-').split(',').collect();
        prop_assert!(!groups[0].is_empty() && groups[0].len() <= 3);
        prop_assert!(groups[1..].iter().all(|g| g.len() == 3));
    }

    #[test]
    fn missing_scopes_ignores_the_prefix(granted in prop::sample::subsequence(oura::SCOPES.to_vec(), 0..=11), prefix in prop::option::of("[a-z]{3,7}")) {
        let text = granted.iter().map(|s| prefix.as_ref().map(|p| format!("{p}:{s}")).unwrap_or_else(|| s.to_string())).collect::<Vec<_>>().join(" ");
        let missing = oura::missing_scopes(Some(&text)).unwrap();
        let expected: Vec<&str> = oura::SCOPES.iter().copied().filter(|s| !granted.contains(s)).collect();
        prop_assert_eq!(missing, expected);
        prop_assert_eq!(oura::missing_scopes(None), None);
    }
}

// ======================================================================
// 7. Robustness: corrupted provider JSON never panics or invents values
// ======================================================================

/// A value of any JSON kind, including the "None" string Eight Sleep sends.
fn arb_leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        Just(json!("None")),
        Just(json!("")),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        prop_oneof![Just(f64::MAX), Just(f64::MIN), Just(-0.0), -1e7f64..1e7].prop_map(Value::from),
        "[ -~]{0,30}".prop_map(Value::from),
        Just(json!("2026-10-18T03:38:00.000Z")),
        Just(json!("2026-10-18")),
        "[1-4x]{0,40}".prop_map(Value::from),
        Just(json!([])),
        Just(json!({})),
    ]
}

const OURA_KEYS: &[&str] = &[
    "id", "day", "type", "bedtime_start", "bedtime_end", "time_in_bed", "total_sleep_duration", "deep_sleep_duration",
    "rem_sleep_duration", "light_sleep_duration", "awake_time", "latency", "efficiency", "average_heart_rate",
    "lowest_heart_rate", "average_hrv", "average_breath", "restless_periods", "sleep_phase_30_sec", "movement_30_sec",
];

/// A valid document with one field corrupted or removed, so everything
/// else the code needs to reach its deeper branches is still in place.
fn arb_corrupted_period() -> impl Strategy<Value = Value> {
    (arb_period(), prop::sample::select(OURA_KEYS), arb_leaf(), any::<bool>()).prop_map(|(p, key, junk, remove)| {
        let mut doc = p.doc();
        if remove {
            doc.as_object_mut().unwrap().remove(key);
        } else {
            doc[key] = junk;
        }
        doc
    })
}

proptest! {
    #[test]
    fn parsers_never_panic(s in ".*", state in "[a-f0-9]{0,64}", body in prop::collection::vec(any::<u8>(), 0..200)) {
        let _ = s.parse::<DaySpec>();
        let _ = oura::parse_callback(&s, &state);
        let _ = oura::parse_pasted(&s, &state);
        let _ = somno::http::short_message(&body);
    }

    /// Whatever field is corrupted, nothing panics, and a reported asleep
    /// time is that document's own value.
    #[test]
    fn corrupted_documents_never_panic_or_invent(doc in arb_corrupted_period(), width in 1usize..200) {
        let day = views::doc_day(&doc).unwrap_or_else(|| d("2026-10-18"));
        let nights = oura::summarize(std::slice::from_ref(&doc), &[], &[], &[day]);
        if let Some(v) = nights.get(&day).and_then(|n| n.asleep_s) {
            prop_assert_eq!(doc["total_sleep_duration"].as_f64(), Some(v));
        }
        if let Some(h) = Hypnogram::from_doc(&doc) {
            let _ = render_chart(&h, width);
            let _ = events(&h, doc["latency"].as_f64());
        }
        let _ = somno::stages::render_period(&doc, width, true);
        let _ = period_json(&doc);
        let _ = views::render_detail(&[], &doc, ny());
        let _ = views::render_table(&views::view("sleep").unwrap(), std::slice::from_ref(&doc));
        let _ = views::render_day(day, ny(), &BTreeMap::from([("sleep".to_string(), vec![doc.clone()])]), &BTreeMap::new());
    }

    #[test]
    fn eight_tolerates_any_day_shape(days in prop::collection::vec(arb_eight_day(), 0..6), junk in arb_leaf()) {
        let wanted = [d("2026-10-18")];
        let _ = eight::summarize(&json!({"days": days}), &wanted);
        let _ = eight::summarize(&json!({"days": [junk.clone()]}), &wanted);
        prop_assert!(eight::summarize(&junk, &wanted).is_err());
    }
}
