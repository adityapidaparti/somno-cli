//! Hegel generators shared by the property tests.
//!
//! Each generator builds a small model of the truth first (a night as runs
//! of sleep stages, a set of sleep periods across days) and derives the
//! provider JSON from it, so a property can compare what somno computes with
//! what the model says. Domains are as wide as the data can be (any UTC
//! offset, any latency, any finite metric); targeted subsets that matter
//! (tied periods, runs around the 5-minute mark, nights that change timezone)
//! are constructed directly rather than hoped for.

#![allow(dead_code)]

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, TimeZone};
use hegel::extras::chrono as cgs;
use hegel::prelude::*;
use serde_json::{json, Value};

pub fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

/// Any day in the years Oura and Eight Sleep can have data for.
pub fn any_day() -> cgs::NaiveDateGenerator {
    cgs::naive_dates().min_value(date(2000, 1, 1)).max_value(date(2100, 12, 31))
}

/// A day from a ten-day window (so documents share days and ranges overlap
/// them), a DST switch or year boundary, or any day.
#[hegel::composite]
pub fn days(tc: &TestCase) -> NaiveDate {
    tc.draw(hegel::one_of!(
        gs::integers::<i64>().min_value(0).max_value(9).map(|n| date(2026, 10, 12) + Duration::days(n)),
        gs::sampled_from(&[date(2026, 3, 8), date(2026, 11, 1), date(2026, 12, 31), date(2027, 1, 1)][..]),
        any_day(),
    ))
}

/// The owner's zones (US Eastern summer/winter, UTC, Central Europe), or any
/// offset an RFC 3339 timestamp can carry: whole minutes within a day
/// (the format has no seconds field for offsets).
#[hegel::composite]
pub fn offsets(tc: &TestCase) -> FixedOffset {
    tc.draw(hegel::one_of!(
        gs::sampled_from(&[-4 * 3600, -5 * 3600, 0, 3600, 2 * 3600][..]).map(|s: i32| FixedOffset::east_opt(s).unwrap()),
        cgs::fixed_offsets().map(|o: FixedOffset| FixedOffset::east_opt(o.local_minus_utc() / 60 * 60).unwrap()),
    ))
}

/// Oura stage codes: deep, light, REM, awake.
pub const CODES: [char; 4] = ['1', '2', '3', '4'];

/// A night as runs of (stage code, 30-second epochs). Lengths cluster around
/// the 10-epoch (5-minute) stretch threshold, with long runs too. Adjacent
/// runs may repeat a stage, so the decoder must merge them.
#[hegel::composite]
pub fn stage_runs(tc: &TestCase) -> Vec<(char, usize)> {
    let count = tc.draw(gs::integers::<usize>().max_value(30));
    (0..count)
        .map(|_| {
            let code = tc.draw(gs::sampled_from(&CODES[..]));
            let len = tc.draw(hegel::one_of!(
                gs::integers::<usize>().min_value(1).max_value(12),
                gs::integers::<usize>().min_value(1).max_value(1200),
            ));
            (code, len)
        })
        .collect()
}

/// One Oura sleep period, with its model.
#[derive(Debug, Clone)]
pub struct Period {
    pub id: String,
    pub day: NaiveDate,
    pub kind: String,
    pub start: DateTime<FixedOffset>,
    pub runs: Vec<(char, usize)>,
    pub movement: Vec<char>,
    /// Oura's latency in seconds; any finite value, including nonsense.
    pub latency: Option<f64>,
    /// Offset the period ends in (differs after travel or a DST switch).
    pub end_offset: FixedOffset,
    pub metrics: [Option<f64>; 6],
}

pub const METRIC_FIELDS: [&str; 6] =
    ["average_hrv", "lowest_heart_rate", "average_heart_rate", "average_breath", "efficiency", "restless_periods"];

pub const KINDS: [&str; 5] = ["long_sleep", "sleep", "late_nap", "rest", "deleted"];

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

    /// Whether Oura counts this period as sleep (not deleted, not a rejected `rest`).
    pub fn confirmed(&self) -> bool {
        !matches!(self.kind.as_str(), "deleted" | "rest")
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
            "latency": self.latency,
            "sleep_phase_30_sec": self.codes(),
            "movement_30_sec": self.movement.iter().collect::<String>(),
        });
        for (field, value) in METRIC_FIELDS.iter().zip(self.metrics) {
            doc[*field] = value.map(Value::from).unwrap_or(Value::Null);
        }
        doc
    }
}

/// Any finite number a provider could send.
pub fn finite() -> gs::FloatGenerator<f64> {
    gs::floats::<f64>().allow_nan(false).allow_infinity(false)
}

#[hegel::composite]
pub fn period(tc: &TestCase) -> Period {
    let day = tc.draw(days());
    let kind = tc.draw(gs::sampled_from(&KINDS[..])).to_string();
    // Bedtime between 18:00 the evening before and 10:00 on the day, at any
    // millisecond; sometimes starting in one offset and ending in another.
    let minutes = tc.draw(gs::integers::<i64>().min_value(0).max_value(16 * 60));
    let millis = tc.draw(hegel::one_of!(gs::just(0i64), gs::integers::<i64>().min_value(0).max_value(999)));
    let offset = tc.draw(offsets());
    let end_offset = if tc.draw(gs::weighted_booleans(0.2)) { tc.draw(offsets()) } else { offset };
    let runs = tc.draw(stage_runs());
    let epochs: usize = runs.iter().map(|(_, n)| n).sum();
    let movement = tc.draw(gs::vecs(gs::sampled_from(&CODES[..])).min_size(epochs).max_size(epochs));
    // Latency: usually within the night in whole epochs, sometimes any finite
    // number (negative, fractional, absurd), sometimes missing.
    let latency = tc.draw(hegel::one_of!(
        gs::just(None),
        gs::integers::<u32>().max_value(60).map(|e| Some(f64::from(e) * 30.0)),
        finite().map(Some),
    ));
    let metrics = [(); 6].map(|_| tc.draw(gs::optional(hegel::one_of!(gs::sampled_from(&[40.0, 52.0, 60.0, 75.0][..]), finite()))));
    let local = day.pred_opt().unwrap().and_hms_opt(18, 0, 0).unwrap() + Duration::minutes(minutes);
    Period {
        id: tc.draw(gs::from_regex(r"[a-f0-9]{8}")),
        day,
        kind,
        start: offset.from_local_datetime(&local).unwrap() + Duration::milliseconds(millis),
        runs,
        movement,
        latency,
        end_offset,
        metrics,
    }
}

/// Several periods. Often one is duplicated under another id, so two
/// periods tie exactly (same day, type, length and times): ties are where
/// the nightly pick could depend on API order.
#[hegel::composite]
pub fn periods(tc: &TestCase) -> Vec<Period> {
    let mut ps: Vec<Period> = tc.draw(gs::vecs(period()).max_size(8));
    if !ps.is_empty() && tc.draw(gs::weighted_booleans(0.35)) {
        let which = tc.draw(gs::integers::<usize>().max_value(ps.len() - 1));
        let mut copy = ps[which].clone();
        copy.id = tc.draw(gs::from_regex(r"[g-z]{8}"));
        tc.event("a period is duplicated under another id");
        ps.push(copy);
    }
    ps
}

/// A document of `collection` as Oura shapes it: day documents carry `day`;
/// rest-mode periods and enhanced tags carry `start_day` and an optional
/// `end_day`. Ids repeat, so duplicates occur.
#[hegel::composite]
pub fn collection_docs(tc: &TestCase, collection: &'static str) -> Value {
    let id = tc.draw(gs::sampled_from(&["a", "b", "c", "d"][..]));
    let first = tc.draw(days());
    if collection == "sleep" {
        json!({"id": id, "day": first.to_string()})
    } else {
        let len = tc.draw(gs::optional(gs::integers::<i64>().min_value(0).max_value(400)));
        json!({"id": id, "start_day": first.to_string(), "end_day": len.map(|l| (first + Duration::days(l)).to_string())})
    }
}

/// A heart-rate sample at any instant, often within two hours of a DST switch
/// (US Eastern, or Lord Howe's 30-minute ones), written in any UTC offset,
/// sometimes with a null bpm.
#[hegel::composite]
pub fn hr_samples(tc: &TestCase) -> Value {
    let utc = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc);
    let switches = [
        utc("2026-03-08T07:00:00Z"),
        utc("2026-11-01T06:00:00Z"),
        utc("2026-04-04T15:00:00Z"),
        utc("2026-10-03T15:30:00Z"),
    ];
    let instant = if tc.draw(gs::booleans()) {
        let switch = tc.draw(gs::sampled_from(&switches[..]));
        tc.event("sample near a DST switch");
        switch + Duration::seconds(tc.draw(gs::integers::<i64>().min_value(-7200).max_value(7200)))
    } else {
        let base = tc.draw(hegel::one_of!(gs::just(date(2026, 10, 12)), any_day()));
        chrono::Utc.from_utc_datetime(&base.and_hms_opt(0, 0, 0).unwrap()) + Duration::seconds(tc.draw(gs::integers::<i64>().min_value(0).max_value(3 * 24 * 3600)))
    };
    let millis = tc.draw(hegel::one_of!(gs::just(0i64), gs::integers::<i64>().min_value(0).max_value(999)));
    let offset = tc.draw(offsets());
    let bpm = tc.draw(gs::optional(gs::integers::<u32>().min_value(25).max_value(230)));
    let source = tc.draw(gs::sampled_from(&["awake", "rest", "sleep", "session", "workout", "live"][..]));
    let instant = instant + Duration::milliseconds(millis);
    json!({"bpm": bpm, "source": source, "timestamp": instant.with_timezone(&offset).to_rfc3339()})
}

/// A value in a provider field: a number, null, the "None" string Eight
/// Sleep sends, or any other JSON.
#[hegel::composite]
pub fn field_values(tc: &TestCase) -> Value {
    tc.draw(hegel::one_of!(
        finite().map(Value::from),
        gs::just(Value::Null),
        gs::just(json!("None")),
        hegel::extras::serde_json::values(),
    ))
}

/// A night from Eight Sleep's trends endpoint in the shape of real
/// responses: day-level fields, sometimes a main session with a stageSummary
/// and sample series, and scores with `current` and `average`.
#[hegel::composite]
pub fn eight_days(tc: &TestCase) -> Value {
    let day = tc.draw(days());
    let mut d = json!({
        "day": day.to_string(),
        "presenceDuration": tc.draw(field_values()),
        "sleepDuration": tc.draw(field_values()),
        "deepDuration": tc.draw(field_values()),
        "remDuration": tc.draw(field_values()),
        "lightDuration": tc.draw(field_values()),
        "score": tc.draw(field_values()),
        "processing": tc.draw(gs::optional(gs::booleans())),
        "sleepQualityScore": {
            "hrv": {"current": tc.draw(field_values()), "average": tc.draw(field_values())},
            "heartRate": {"current": tc.draw(field_values()), "average": tc.draw(field_values())},
        },
    });
    if tc.draw(gs::booleans()) {
        d["mainSessionId"] = json!("main");
        d["sessions"] = json!([{
            "id": "main",
            "stageSummary": {
                "sleepDuration": tc.draw(field_values()), "awakeDuration": tc.draw(field_values()),
                "outDuration": tc.draw(field_values()), "totalDuration": tc.draw(field_values()),
            },
            "timeseries": {
                "heartRate": [["2026-10-18T04:00:00.000Z", tc.draw(field_values())]],
                "tempBedC": [["2026-10-18T04:00:00.000Z", tc.draw(field_values())]],
            },
        }]);
    }
    d
}
