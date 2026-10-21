//! Normalized per-night summary shared by both sources, plus terminal rendering.
//!
//! Every metric is an `Option`: absent means the source did not report it,
//! never zero. Nights are keyed by each source's own day label; the two
//! providers' labels are not re-derived or forced to agree.

use chrono::{DateTime, FixedOffset, NaiveDate, Offset};
use chrono_tz::Tz;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::types::fmt_duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Oura,
    EightSleep,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Oura => "Oura",
            Source::EightSleep => "Eight Sleep",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Source::Oura => "oura",
            Source::EightSleep => "eight_sleep",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Night {
    pub source_id: Option<String>,
    /// Oura's sleep period type (`long_sleep`, `sleep`, …); none for Eight Sleep.
    pub period_type: Option<String>,
    pub bedtime_start: Option<DateTime<FixedOffset>>,
    pub bedtime_end: Option<DateTime<FixedOffset>>,
    pub time_in_bed_s: Option<f64>,
    pub asleep_s: Option<f64>,
    pub deep_s: Option<f64>,
    pub rem_s: Option<f64>,
    pub light_s: Option<f64>,
    pub awake_s: Option<f64>,
    pub latency_s: Option<f64>,
    pub efficiency_pct: Option<f64>,
    pub hr_avg_bpm: Option<f64>,
    pub hr_lowest_bpm: Option<f64>,
    pub hrv_avg_ms: Option<f64>,
    pub breath_avg_per_min: Option<f64>,
    pub sleep_score: Option<f64>,
    pub readiness_score: Option<f64>,
    pub temp_deviation_c: Option<f64>,
    pub bed_temp_c: Option<f64>,
    pub room_temp_c: Option<f64>,
    /// Oura `restless_periods`.
    pub restless_periods: Option<f64>,
    /// Eight Sleep `tnt` (toss and turns).
    pub toss_turns: Option<f64>,
    /// Eight Sleep: time in bed's presence span spent out of bed.
    pub out_of_bed_s: Option<f64>,
    /// Eight Sleep still computing this night.
    pub processing: Option<bool>,
    /// Further sleep periods on this day (naps, rest) that this summary leaves out.
    pub other_periods: usize,
}

/// What one source has for one day.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Slot {
    Record { night: Box<Night> },
    NoRecord,
    Unavailable { reason: String },
}

/// One source's result over the requested days: the nights it returned, or why
/// it could not be read at all.
pub type SourceNights = Result<BTreeMap<NaiveDate, Night>, String>;

pub fn slot(nights: &SourceNights, day: NaiveDate) -> Slot {
    match nights {
        Err(reason) => Slot::Unavailable { reason: reason.clone() },
        Ok(map) => match map.get(&day) {
            Some(night) => Slot::Record { night: Box::new(night.clone()) },
            None => Slot::NoRecord,
        },
    }
}

// ---- JSON access: numbers only; "None" strings and other types are unknown ----

pub fn at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(v, |cur, key| cur.get(*key))
}

pub fn num(v: &Value, path: &[&str]) -> Option<f64> {
    at(v, path).and_then(Value::as_f64).filter(|x| x.is_finite())
}

/// First present number among several candidate paths.
pub fn num_any(v: &Value, paths: &[&[&str]]) -> Option<f64> {
    paths.iter().find_map(|p| num(v, p))
}

pub fn text<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    at(v, path).and_then(Value::as_str).filter(|s| !s.is_empty() && *s != "None")
}

pub fn day_of(v: &Value) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text(v, &["day"])?, "%Y-%m-%d").ok()
}

// ---- rendering ----

const DASH: &str = "—";

/// Clock times as you lived them. Oura stamps each period with the phone's
/// UTC offset at the time, so it is kept (a bedtime abroad stays 02:00, not
/// 20:00 at home) and shown when it differs from the display zone. Eight
/// Sleep sends UTC, which says nothing about where you were, so it is
/// converted to the display zone.
fn hm(t: &DateTime<FixedOffset>, tz: Tz, source: Source) -> String {
    match source {
        Source::Oura => t.format("%H:%M").to_string(),
        Source::EightSleep => t.with_timezone(&tz).format("%H:%M").to_string(),
    }
}

fn span(n: &Night, tz: Tz, source: Source) -> Option<String> {
    let clock = |t: &Option<DateTime<FixedOffset>>| t.as_ref().map(|t| hm(t, tz, source)).unwrap_or_else(|| "?".into());
    if n.bedtime_start.is_none() && n.bedtime_end.is_none() {
        return None;
    }
    let mut out = format!("{} → {}", clock(&n.bedtime_start), clock(&n.bedtime_end));
    if source == Source::Oura {
        if let Some(t) = n.bedtime_end.as_ref().or(n.bedtime_start.as_ref()) {
            let home = t.with_timezone(&tz).offset().fix();
            if *t.offset() != home {
                out += &format!(" ({})", t.format("%:z"));
            }
        }
    }
    Some(out)
}

fn dur(x: Option<f64>) -> Option<String> {
    x.and_then(fmt_duration)
}

fn unit(x: Option<f64>, decimals: usize, suffix: &str) -> Option<String> {
    x.map(|v| format!("{v:.decimals$}{suffix}"))
}

type Cell = fn(&Night, Tz, Source) -> Option<String>;

fn rows() -> Vec<(&'static str, Cell)> {
    vec![
        ("in bed", span),
        ("time in bed", |n, _, _| dur(n.time_in_bed_s)),
        ("asleep", |n, _, _| dur(n.asleep_s)),
        ("  deep", |n, _, _| dur(n.deep_s)),
        ("  REM", |n, _, _| dur(n.rem_s)),
        ("  light", |n, _, _| dur(n.light_s)),
        ("awake", |n, _, _| dur(n.awake_s)),
        ("out of bed", |n, _, _| dur(n.out_of_bed_s)),
        ("latency", |n, _, _| dur(n.latency_s)),
        ("efficiency", |n, _, _| unit(n.efficiency_pct, 0, "%")),
        ("HR avg", |n, _, _| unit(n.hr_avg_bpm, 0, " bpm")),
        ("HR lowest", |n, _, _| unit(n.hr_lowest_bpm, 0, " bpm")),
        ("HRV avg", |n, _, _| unit(n.hrv_avg_ms, 0, " ms")),
        ("breathing", |n, _, _| unit(n.breath_avg_per_min, 1, " /min")),
        ("sleep score", |n, _, _| unit(n.sleep_score, 0, "")),
        ("readiness", |n, _, _| unit(n.readiness_score, 0, "")),
        ("temp deviation", |n, _, _| n.temp_deviation_c.map(|v| format!("{v:+.2} °C"))),
        ("bed temp", |n, _, _| unit(n.bed_temp_c, 1, " °C")),
        ("room temp", |n, _, _| unit(n.room_temp_c, 1, " °C")),
        ("restless periods", |n, _, _| unit(n.restless_periods, 0, "")),
        ("toss & turns", |n, _, _| unit(n.toss_turns, 0, "")),
        ("note", |n, _, _| {
            let mut notes = Vec::new();
            if n.processing == Some(true) {
                notes.push("still processing".to_string());
            }
            if let Some(t) = n.period_type.as_deref().filter(|t| *t != "long_sleep") {
                notes.push(format!("period type {t}"));
            }
            if n.other_periods > 0 {
                notes.push(format!("+{} other period(s)", n.other_periods));
            }
            (!notes.is_empty()).then(|| notes.join(", "))
        }),
    ]
}

fn slot_header(slot: &Slot) -> Option<&'static str> {
    match slot {
        Slot::Record { .. } => None,
        Slot::NoRecord => Some("no record"),
        Slot::Unavailable { .. } => Some("unavailable"),
    }
}

/// Side-by-side view of one night. Rows where no column has a value are omitted.
pub fn render_night(day: NaiveDate, tz: Tz, columns: &[(Source, Slot)]) -> String {
    const LABEL: usize = 18;
    const COL: usize = 18;
    let mut out = format!("Night ending {} ({})\n", day.format("%a %Y-%m-%d"), tz.name());
    out += &format!("{:LABEL$}", "");
    for (source, _) in columns {
        out += &format!("{:<COL$}", source.label());
    }
    out = out.trim_end().to_string() + "\n";
    if columns.iter().any(|(_, s)| slot_header(s).is_some()) {
        out += &format!("{:LABEL$}", "");
        for (_, slot) in columns {
            out += &format!("{:<COL$}", slot_header(slot).unwrap_or(""));
        }
        out = out.trim_end().to_string() + "\n";
    }
    for (label, cell) in rows() {
        let values: Vec<Option<String>> = columns
            .iter()
            .map(|(source, slot)| match slot {
                Slot::Record { night } => cell(night, tz, *source),
                _ => None,
            })
            .collect();
        if values.iter().all(Option::is_none) {
            continue;
        }
        let mut line = format!("{label:LABEL$}");
        for v in values {
            line += &format!("{:<COL$}", v.as_deref().unwrap_or(DASH));
        }
        out += line.trim_end();
        out.push('\n');
    }
    out += &footnotes(columns.iter().map(|(s, slot)| (*s, slot)));
    out
}

fn footnotes<'a>(slots: impl Iterator<Item = (Source, &'a Slot)>) -> String {
    let mut out = String::new();
    for (source, slot) in slots {
        if let Slot::Unavailable { reason } = slot {
            out += &format!("{}: {reason}\n", source.label());
        }
    }
    out
}

/// One row per night per source.
pub fn render_table(days: &[NaiveDate], tz: Tz, sources: &[(Source, SourceNights)]) -> String {
    let header = ["day", "source", "in bed", "asleep", "deep", "REM", "HRV", "HR low", "score"];
    let widths = [12, 13, 24, 9, 9, 9, 6, 8, 5];
    let pad = |cells: &[String]| -> String {
        let line: String = cells.iter().zip(widths).map(|(c, w)| format!("{c:<w$}")).collect();
        line.trim_end().to_string() + "\n"
    };
    let mut out = format!("{} → {} ({})\n", days.first().map(|d| d.to_string()).unwrap_or_default(),
        days.last().map(|d| d.to_string()).unwrap_or_default(), tz.name());
    out += &pad(&header.map(String::from));
    for day in days {
        let mut first = true;
        for (source, nights) in sources {
            let Ok(map) = nights else { continue };
            let day_cell = if first { day.to_string() } else { String::new() };
            first = false;
            let cells = match map.get(day) {
                None => vec![day_cell, source.label().into(), "no record".into()],
                Some(n) => {
                    let or = |v: Option<String>| v.unwrap_or_else(|| DASH.into());
                    vec![
                        day_cell,
                        source.label().into(),
                        or(span(n, tz, *source)),
                        or(dur(n.asleep_s)),
                        or(dur(n.deep_s)),
                        or(dur(n.rem_s)),
                        or(unit(n.hrv_avg_ms, 0, "")),
                        or(unit(n.hr_lowest_bpm, 0, "")),
                        or(unit(n.sleep_score, 0, "")),
                    ]
                }
            };
            out += &pad(&cells);
        }
    }
    for (source, nights) in sources {
        if let Err(reason) = nights {
            out += &format!("{}: {reason}\n", source.label());
        }
    }
    out
}
