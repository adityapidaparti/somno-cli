//! Generators shared by the property tests.
//!
//! Correct by construction: each generator first builds a small model of the
//! truth (a night as runs of sleep stages, a set of sleep periods, a set of
//! dated documents) and derives the provider JSON from it. Properties then
//! compare what the code computes with what the model says.
//!
//! Values come from small domains on purpose, so the cases that hide bugs
//! happen often: two periods with the same length on the same day, runs right
//! at the 5-minute threshold, bedtimes either side of midnight, DST switch
//! days, and nights that start in one UTC offset and end in another.

#![allow(dead_code)]

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, TimeZone};
use proptest::prelude::*;
use serde_json::{json, Value};

pub fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

/// Mostly a ten-day window, so documents share days; sometimes a DST switch
/// day (US) or a year boundary.
pub fn arb_day() -> impl Strategy<Value = NaiveDate> {
    prop_oneof![
        5 => (0i64..10).prop_map(|n| date(2026, 10, 12) + Duration::days(n)),
        1 => prop::sample::select(vec![date(2026, 3, 8), date(2026, 11, 1), date(2026, 12, 31), date(2027, 1, 1)]),
    ]
}

/// Mostly the owner's zones (US Eastern summer/winter, UTC, Central Europe
/// winter/summer); sometimes any quarter hour from -12:00 to +14:00.
pub fn arb_offset() -> impl Strategy<Value = FixedOffset> {
    prop_oneof![
        3 => prop::sample::select(vec![-4 * 3600, -5 * 3600, 0, 3600, 2 * 3600]),
        1 => (-48i32..=56).prop_map(|q| q * 900),
    ]
    .prop_map(|secs| FixedOffset::east_opt(secs).unwrap())
}

/// Oura stage codes: deep, light, REM, awake.
pub const CODES: [char; 4] = ['1', '2', '3', '4'];

/// A night as runs of (stage code, 30-second epochs). Most runs are 1–12
/// epochs, around the 10-epoch (5-minute) stretch threshold; some are long.
/// Adjacent runs may repeat a stage, so the model must merge them too.
pub fn arb_runs() -> impl Strategy<Value = Vec<(char, usize)>> {
    let len = prop_oneof![4 => 1usize..=12, 1 => 1usize..=240];
    prop::collection::vec((prop::sample::select(CODES.to_vec()), len), 0..30)
}

/// Optional metric from a small domain, so values tie across periods; `None`
/// becomes JSON null.
fn arb_metric() -> impl Strategy<Value = Option<f64>> {
    prop::option::weighted(0.8, prop::sample::select(vec![40.0, 52.0, 52.5, 60.0, 75.0, 91.0]))
}

/// One Oura sleep period, with its model.
#[derive(Debug, Clone)]
pub struct Period {
    pub id: String,
    pub day: NaiveDate,
    pub kind: &'static str,
    pub start: DateTime<FixedOffset>,
    pub runs: Vec<(char, usize)>,
    pub movement: Vec<char>,
    pub latency_epochs: Option<usize>,
    /// Offset the period ends in (differs after travel or a DST switch).
    pub end_offset: FixedOffset,
    pub metrics: [Option<f64>; 6],
}

pub const METRIC_FIELDS: [&str; 6] =
    ["average_hrv", "lowest_heart_rate", "average_heart_rate", "average_breath", "efficiency", "restless_periods"];

impl Period {
    pub fn codes(&self) -> String {
        self.runs.iter().map(|(c, n)| c.to_string().repeat(*n)).collect()
    }

    pub fn epochs(&self) -> usize {
        self.runs.iter().map(|(_, n)| n).sum()
    }

    pub fn secs(&self, code: char) -> i64 {
        self.runs.iter().filter(|(c, _)| *c == code).map(|(_, n)| *n as i64 * 30).sum()
    }

    pub fn asleep_secs(&self) -> i64 {
        self.secs('1') + self.secs('2') + self.secs('3')
    }

    pub fn end(&self) -> DateTime<FixedOffset> {
        (self.start + Duration::seconds(self.epochs() as i64 * 30)).with_timezone(&self.end_offset)
    }

    /// The Oura document, consistent with the model.
    pub fn doc(&self) -> Value {
        let mut doc = json!({
            "id": self.id,
            "day": self.day.to_string(),
            "type": self.kind,
            "bedtime_start": self.start.to_rfc3339(),
            "bedtime_end": self.end().to_rfc3339(),
            "time_in_bed": self.epochs() * 30,
            "total_sleep_duration": self.asleep_secs(),
            "deep_sleep_duration": self.secs('1'),
            "light_sleep_duration": self.secs('2'),
            "rem_sleep_duration": self.secs('3'),
            "awake_time": self.secs('4'),
            "latency": self.latency_epochs.map(|e| e * 30),
            "sleep_phase_30_sec": self.codes(),
            "movement_30_sec": self.movement.iter().collect::<String>(),
        });
        for (field, value) in METRIC_FIELDS.iter().zip(self.metrics) {
            doc[*field] = value.map(Value::from).unwrap_or(Value::Null);
        }
        doc
    }
}

pub fn arb_period() -> impl Strategy<Value = Period> {
    let kind = prop::sample::select(vec!["long_sleep", "long_sleep", "long_sleep", "sleep", "late_nap", "rest", "deleted"]);
    (
        arb_day(),
        kind,
        0i64..(16 * 60),
        arb_offset(),
        prop::option::weighted(0.15, arb_offset()),
        arb_runs(),
        prop::option::of(0usize..=40),
        "[a-f0-9]{8}",
        [arb_metric(), arb_metric(), arb_metric(), arb_metric(), arb_metric(), arb_metric()],
        prop_oneof![3 => Just(0i64), 1 => 0i64..1000],
    )
        .prop_flat_map(|(day, kind, minutes, offset, travel, runs, latency, id, metrics, millis)| {
            let epochs: usize = runs.iter().map(|(_, n)| n).sum();
            let movement = prop::collection::vec(prop::sample::select(CODES.to_vec()), epochs);
            (Just((day, kind, minutes, offset, travel, runs, latency, id, metrics, millis)), movement)
        })
        .prop_map(|((day, kind, minutes, offset, travel, runs, latency, id, metrics, millis), movement)| {
            // Bedtime between 18:00 the evening before and 10:00 on the day,
            // sometimes with sub-second precision (Oura sends milliseconds).
            let local = day.pred_opt().unwrap().and_hms_opt(18, 0, 0).unwrap() + Duration::minutes(minutes);
            Period {
                id,
                day,
                kind,
                start: offset.from_local_datetime(&local).unwrap() + Duration::milliseconds(millis),
                runs,
                movement,
                latency_epochs: latency,
                end_offset: travel.unwrap_or(offset),
                metrics,
            }
        })
}

/// Several periods; often one is duplicated under another id, so two periods
/// tie exactly (same day, type, length and times).
pub fn arb_periods() -> impl Strategy<Value = Vec<Period>> {
    (prop::collection::vec(arb_period(), 0..8), any::<prop::sample::Index>(), prop::bool::weighted(0.35), "[g-z]{8}").prop_map(
        |(mut periods, which, duplicate, id)| {
            if duplicate && !periods.is_empty() {
                let mut copy = periods[which.index(periods.len())].clone();
                copy.id = id;
                periods.push(copy);
            }
            periods
        },
    )
}

/// A document of `collection` as Oura shapes it: day documents carry `day`;
/// rest-mode periods and enhanced tags carry `start_day` and an optional
/// `end_day`. Ids repeat sometimes, so duplicates occur.
pub fn arb_collection_doc(collection: &'static str) -> BoxedStrategy<Value> {
    let id = prop::sample::select(vec!["a", "b", "c", "d", "e", "f", "g", "h"]);
    if matches!(collection, "rest_mode_period" | "enhanced_tag") {
        (arb_day(), prop::option::weighted(0.6, 0i64..40), id)
            .prop_map(|(d, len, id)| json!({"id": id, "start_day": d.to_string(), "end_day": len.map(|l| (d + Duration::days(l)).to_string())}))
            .boxed()
    } else {
        (arb_day(), id).prop_map(|(d, id)| json!({"id": id, "day": d.to_string()})).boxed()
    }
}

/// A heart-rate sample at an instant near a US DST switch or in the
/// ten-day window, written in an arbitrary UTC offset.
pub fn arb_hr_sample() -> impl Strategy<Value = Value> {
    let base = prop::sample::select(vec![
        date(2026, 3, 7), date(2026, 10, 31), date(2026, 10, 12), date(2026, 10, 16),
    ]);
    (
        base,
        0i64..(3 * 24 * 3600),
        prop_oneof![3 => Just(0i64), 1 => 0i64..1000],
        arb_offset(),
        prop::option::weighted(0.9, 35u32..190),
        prop::sample::select(vec!["awake", "rest", "sleep", "session", "workout", "live"]),
    )
        .prop_map(|(day, secs, millis, offset, bpm, source)| {
            let instant = chrono::Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0).unwrap()) + Duration::seconds(secs) + Duration::milliseconds(millis);
            json!({
                "bpm": bpm,
                "source": source,
                "timestamp": instant.with_timezone(&offset).to_rfc3339(),
            })
        })
}

/// A night from Eight Sleep's trends endpoint, in the shape of real
/// responses: day-level fields, sometimes a main session with a stageSummary
/// and sample series, scores with `current` and `average`. Fields come from
/// small domains and are sometimes null or the string "None", as the
/// unofficial API sends.
pub fn arb_eight_day() -> impl Strategy<Value = Value> {
    let secs = || {
        prop_oneof![
            4 => prop::sample::select(vec![0i64, 1020, 3600, 7200, 21600, 25200, 28800]).prop_map(Value::from),
            1 => Just(Value::Null),
            1 => Just(json!("None")),
        ]
    };
    let small = || prop::option::of(prop::sample::select(vec![14.0, 15.5, 52.0, 58.0, 85.0])).prop_map(|v| v.map(Value::from).unwrap_or(Value::Null));
    let session = (secs(), secs(), secs(), secs(), small()).prop_map(|(sleep, awake, out, total, hr)| {
        json!({"id": "main", "stageSummary": {"sleepDuration": sleep, "awakeDuration": awake, "outDuration": out, "totalDuration": total},
               "timeseries": {"heartRate": [["2026-10-18T04:00:00.000Z", hr]], "tempBedC": [["2026-10-18T04:00:00.000Z", 30.5]]}})
    });
    (arb_day(), secs(), secs(), secs(), secs(), secs(), small(), small(), small(), prop::option::of(any::<bool>()), prop::option::weighted(0.5, session))
        .prop_map(|(day, presence, sleep, deep, rem, light, hrv, hr, score, processing, session)| {
            let mut d = json!({
                "day": day.to_string(),
                "presenceDuration": presence,
                "sleepDuration": sleep,
                "deepDuration": deep,
                "remDuration": rem,
                "lightDuration": light,
                "score": score,
                "processing": processing,
                "sleepQualityScore": {"hrv": {"current": hrv, "average": 61}, "heartRate": {"current": hr, "average": 55}},
            });
            if let Some(s) = session {
                d["mainSessionId"] = json!("main");
                d["sessions"] = json!([s]);
            }
            d
        })
}
