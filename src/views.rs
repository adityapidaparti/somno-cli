//! Readable views of every Oura data collection except the ring's battery and
//! hardware (those stay under `somno raw`). A table per range of days, a full
//! detail view for one day, and a one-screen `day` dashboard.
//!
//! Detail views render every field in Oura's documents, formatted by field
//! name (durations, kcal, °C, bpm, samples), so new fields Oura adds still
//! show up. Field meanings follow Oura OpenAPI 1.42 (checked 2026-10-20).

use chrono::{DateTime, FixedOffset, NaiveDate, Offset, Timelike};
use chrono_tz::Tz;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

use crate::night::{num, text};
use crate::types::{fmt_duration, parse_timestamp};

const DASH: &str = "—";

pub type Cell = fn(&Value) -> Option<String>;

pub struct Column {
    pub header: &'static str,
    pub width: usize,
    pub cell: Cell,
}

pub struct View {
    pub name: &'static str,
    pub title: &'static str,
    pub collections: &'static [&'static str],
    /// Fields listed first in the detail view, in this order; the rest follow alphabetically.
    pub first: &'static [&'static str],
    pub columns: Vec<Column>,
}

fn col(header: &'static str, width: usize, cell: Cell) -> Column {
    Column { header, width, cell }
}

// ---- small formatters ----

fn f0(x: Option<f64>) -> Option<String> {
    x.map(|v| format!("{v:.0}"))
}

fn dur(x: Option<f64>) -> Option<String> {
    x.and_then(fmt_duration)
}

pub fn thousands(n: f64) -> String {
    let s = format!("{:.0}", n.abs());
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0.0 { format!("-{out}") } else { out }
}

fn distance(m: f64) -> String {
    if m >= 1000.0 { format!("{:.1} km", m / 1000.0) } else { format!("{m:.0} m") }
}

/// `HH:MM` in the timestamp's own offset (the local time where it happened).
fn clock(s: Option<&str>) -> Option<String> {
    parse_timestamp(s?).map(|t| t.format("%H:%M").to_string())
}

fn clock_span(d: &Value, start: &str, end: &str) -> Option<String> {
    match (clock(text(d, &[start])), clock(text(d, &[end]))) {
        (None, None) => None,
        (a, b) => Some(format!("{}–{}", a.unwrap_or_else(|| "?".into()), b.unwrap_or_else(|| "?".into()))),
    }
}

fn span_secs(d: &Value, start: &str, end: &str) -> Option<f64> {
    let a = parse_timestamp(text(d, &[start])?)?;
    let b = parse_timestamp(text(d, &[end])?)?;
    Some((b - a).num_seconds() as f64).filter(|s| *s >= 0.0)
}

/// Numeric items of an Oura sample object (`interval`, `items`, `timestamp`).
fn sample_items(v: &Value) -> Vec<f64> {
    v.get("items").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default()
}

fn mean(xs: &[f64]) -> Option<f64> {
    (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
}

fn is_sample(m: &Map<String, Value>) -> bool {
    m.contains_key("items") && m.contains_key("interval")
}

/// Oura's recommended bedtime window: offsets in seconds from midnight.
fn bedtime_window(v: &Value) -> Option<String> {
    let hm = |secs: f64| {
        let m = ((secs / 60.0).round() as i64).rem_euclid(24 * 60);
        format!("{:02}:{:02}", m / 60, m % 60)
    };
    Some(format!("{}–{}", hm(num(v, &["start_offset"])?), hm(num(v, &["end_offset"])?)))
}

fn tag_name(d: &Value) -> Option<String> {
    if let Some(tags) = d.get("tags").and_then(Value::as_array) {
        let names: Vec<&str> = tags.iter().filter_map(Value::as_str).collect();
        if !names.is_empty() {
            return Some(names.join(", "));
        }
    }
    match text(d, &["tag_type_code"]) {
        Some("custom") | None => text(d, &["custom_name"]).map(str::to_string),
        Some(code) => Some(code.trim_start_matches("tag_generic_").replace('_', " ")),
    }
}

// ---- the views ----

pub fn views() -> Vec<View> {
    vec![
        View {
            name: "readiness",
            title: "Readiness",
            collections: &["daily_readiness"],
            first: &["day", "score", "temperature_deviation", "temperature_trend_deviation", "contributors"],
            columns: vec![
                col("score", 6, |d| f0(num(d, &["score"]))),
                col("temp dev", 10, |d| num(d, &["temperature_deviation"]).map(|v| format!("{v:+.2} °C"))),
                col("trend", 10, |d| num(d, &["temperature_trend_deviation"]).map(|v| format!("{v:+.2} °C"))),
                col("HRV bal", 8, |d| f0(num(d, &["contributors", "hrv_balance"]))),
                col("RHR score", 10, |d| f0(num(d, &["contributors", "resting_heart_rate"]))),
                col("recovery", 9, |d| f0(num(d, &["contributors", "recovery_index"]))),
                col("sleep bal", 10, |d| f0(num(d, &["contributors", "sleep_balance"]))),
                col("activity bal", 12, |d| f0(num(d, &["contributors", "activity_balance"]))),
            ],
        },
        View {
            name: "sleep",
            title: "Sleep periods",
            collections: &["sleep"],
            first: &[
                "day", "type", "bedtime_start", "bedtime_end", "time_in_bed", "total_sleep_duration",
                "deep_sleep_duration", "rem_sleep_duration", "light_sleep_duration", "awake_time", "latency",
                "efficiency", "average_heart_rate", "lowest_heart_rate", "average_hrv", "average_breath",
                "restless_periods", "readiness",
            ],
            columns: vec![
                col("type", 11, |d| text(d, &["type"]).map(str::to_string)),
                col("in bed", 12, |d| clock_span(d, "bedtime_start", "bedtime_end")),
                col("asleep", 8, |d| dur(num(d, &["total_sleep_duration"]))),
                col("deep", 8, |d| dur(num(d, &["deep_sleep_duration"]))),
                col("REM", 8, |d| dur(num(d, &["rem_sleep_duration"]))),
                col("eff", 5, |d| num(d, &["efficiency"]).map(|v| format!("{v:.0}%"))),
                col("HR low", 7, |d| f0(num(d, &["lowest_heart_rate"]))),
                col("HRV", 5, |d| f0(num(d, &["average_hrv"]))),
            ],
        },
        View {
            name: "sleep-score",
            title: "Sleep score",
            collections: &["daily_sleep"],
            first: &["day", "score", "contributors"],
            columns: vec![
                col("score", 6, |d| f0(num(d, &["score"]))),
                col("total", 6, |d| f0(num(d, &["contributors", "total_sleep"]))),
                col("deep", 5, |d| f0(num(d, &["contributors", "deep_sleep"]))),
                col("REM", 5, |d| f0(num(d, &["contributors", "rem_sleep"]))),
                col("effic", 6, |d| f0(num(d, &["contributors", "efficiency"]))),
                col("latency", 8, |d| f0(num(d, &["contributors", "latency"]))),
                col("restful", 8, |d| f0(num(d, &["contributors", "restfulness"]))),
                col("timing", 6, |d| f0(num(d, &["contributors", "timing"]))),
            ],
        },
        View {
            name: "activity",
            title: "Activity",
            collections: &["daily_activity"],
            first: &[
                "day", "score", "steps", "active_calories", "total_calories", "target_calories",
                "equivalent_walking_distance", "high_activity_time", "medium_activity_time", "low_activity_time",
                "sedentary_time", "resting_time", "non_wear_time", "inactivity_alerts", "contributors",
            ],
            columns: vec![
                col("score", 6, |d| f0(num(d, &["score"]))),
                col("steps", 8, |d| num(d, &["steps"]).map(thousands)),
                col("active kcal", 12, |d| num(d, &["active_calories"]).map(thousands)),
                col("total kcal", 11, |d| num(d, &["total_calories"]).map(thousands)),
                col("high", 8, |d| dur(num(d, &["high_activity_time"]))),
                col("medium", 8, |d| dur(num(d, &["medium_activity_time"]))),
                col("low", 8, |d| dur(num(d, &["low_activity_time"]))),
                col("sedentary", 10, |d| dur(num(d, &["sedentary_time"]))),
            ],
        },
        View {
            name: "stress",
            title: "Stress",
            collections: &["daily_stress"],
            first: &["day", "day_summary", "stress_high", "recovery_high"],
            columns: vec![
                col("summary", 10, |d| text(d, &["day_summary"]).map(str::to_string)),
                col("high stress", 12, |d| dur(num(d, &["stress_high"]))),
                col("high recovery", 14, |d| dur(num(d, &["recovery_high"]))),
            ],
        },
        View {
            name: "resilience",
            title: "Resilience",
            collections: &["daily_resilience"],
            first: &["day", "level", "contributors"],
            columns: vec![
                col("level", 12, |d| text(d, &["level"]).map(str::to_string)),
                col("sleep rec", 10, |d| f0(num(d, &["contributors", "sleep_recovery"]))),
                col("daytime rec", 12, |d| f0(num(d, &["contributors", "daytime_recovery"]))),
                col("stress", 6, |d| f0(num(d, &["contributors", "stress"]))),
            ],
        },
        View {
            name: "spo2",
            title: "SpO2",
            collections: &["daily_spo2"],
            first: &["day", "spo2_percentage", "breathing_disturbance_index"],
            columns: vec![
                col("SpO2 avg", 9, |d| num(d, &["spo2_percentage", "average"]).map(|v| format!("{v:.1}%"))),
                col("breathing disturbance", 21, |d| f0(num(d, &["breathing_disturbance_index"]))),
            ],
        },
        View {
            name: "cardio-age",
            title: "Cardiovascular age",
            collections: &["daily_cardiovascular_age"],
            first: &["day", "vascular_age", "pulse_wave_velocity"],
            columns: vec![
                col("vascular age", 13, |d| f0(num(d, &["vascular_age"]))),
                col("pulse wave velocity", 19, |d| num(d, &["pulse_wave_velocity"]).map(|v| format!("{v:.1} m/s"))),
            ],
        },
        View {
            name: "vo2max",
            title: "VO2 max",
            collections: &["vO2_max"],
            first: &["day", "vo2_max", "timestamp"],
            columns: vec![col("VO2 max", 8, |d| f0(num(d, &["vo2_max"])))],
        },
        View {
            name: "workouts",
            title: "Workouts",
            collections: &["workout"],
            first: &["day", "activity", "label", "intensity", "start_datetime", "end_datetime", "calories", "distance", "source"],
            columns: vec![
                col("time", 12, |d| clock_span(d, "start_datetime", "end_datetime")),
                col("activity", 16, |d| text(d, &["label"]).or(text(d, &["activity"])).map(|s| s.replace('_', " "))),
                col("intensity", 10, |d| text(d, &["intensity"]).map(str::to_string)),
                col("duration", 9, |d| dur(span_secs(d, "start_datetime", "end_datetime"))),
                col("kcal", 6, |d| f0(num(d, &["calories"]))),
                col("distance", 9, |d| num(d, &["distance"]).map(distance)),
                col("source", 12, |d| text(d, &["source"]).map(|s| s.replace('_', " "))),
            ],
        },
        View {
            name: "sessions",
            title: "Sessions",
            collections: &["session"],
            first: &["day", "type", "start_datetime", "end_datetime", "mood", "heart_rate", "heart_rate_variability", "motion_count"],
            columns: vec![
                col("time", 12, |d| clock_span(d, "start_datetime", "end_datetime")),
                col("type", 12, |d| text(d, &["type"]).map(|s| s.replace('_', " "))),
                col("duration", 9, |d| dur(span_secs(d, "start_datetime", "end_datetime"))),
                col("mood", 6, |d| text(d, &["mood"]).map(str::to_string)),
                col("HR avg", 7, |d| f0(d.get("heart_rate").and_then(|v| mean(&sample_items(v))))),
                col("HRV avg", 8, |d| f0(d.get("heart_rate_variability").and_then(|v| mean(&sample_items(v))))),
            ],
        },
        View {
            name: "tags",
            title: "Tags",
            collections: &["enhanced_tag", "tag"],
            first: &["day", "start_day", "start_time", "end_day", "end_time", "tag_type_code", "custom_name", "tags", "comment", "text", "timestamp"],
            columns: vec![
                col("time", 12, |d| match text(d, &["end_time"]) {
                    Some(_) => clock_span(d, "start_time", "end_time"),
                    None => clock(text(d, &["start_time"]).or(text(d, &["timestamp"]))),
                }),
                col("tag", 24, tag_name),
                col("note", 30, |d| text(d, &["comment"]).or(text(d, &["text"])).map(str::to_string)),
            ],
        },
        View {
            name: "bedtime",
            title: "Recommended bedtime",
            collections: &["sleep_time"],
            first: &["day", "optimal_bedtime", "recommendation", "status"],
            columns: vec![
                col("window", 12, |d| d.get("optimal_bedtime").and_then(bedtime_window)),
                col("recommendation", 24, |d| text(d, &["recommendation"]).map(|s| s.replace('_', " "))),
                col("status", 24, |d| text(d, &["status"]).map(|s| s.replace('_', " "))),
            ],
        },
        View {
            name: "rest-mode",
            title: "Rest mode",
            collections: &["rest_mode_period"],
            first: &["start_day", "start_time", "end_day", "end_time", "episodes"],
            columns: vec![
                col("until", 12, |d| text(d, &["end_day"]).map(str::to_string).or(Some("ongoing".into()))),
                col("episodes", 9, |d| d.get("episodes").and_then(Value::as_array).map(|a| a.len().to_string())),
            ],
        },
    ]
}

pub fn view(name: &str) -> Option<View> {
    views().into_iter().find(|v| v.name == name)
}

// ---- selecting documents ----

fn parse_day(s: Option<&str>) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s?, "%Y-%m-%d").ok()
}

/// The day a document belongs to (`day`, or `start_day` for spans).
pub fn doc_day(d: &Value) -> Option<NaiveDate> {
    parse_day(text(d, &["day"])).or_else(|| parse_day(text(d, &["start_day"])))
}

/// Whether a document of `collection` falls in `start..=end` (empty when
/// `end < start`). Spans count when they overlap. A missing `end_day` means
/// different things (Oura OpenAPI 1.42): an enhanced tag without one has no
/// duration and belongs to its start day; a rest-mode period without one is
/// still going.
pub fn in_range(collection: &str, d: &Value, start: NaiveDate, end: NaiveDate) -> bool {
    if end < start {
        return false;
    }
    let Some(first) = doc_day(d) else { return false };
    // No `day` means the day came from `start_day`: a span.
    if d.get("day").is_none() {
        let open_end = if collection == "enhanced_tag" { first } else { NaiveDate::MAX };
        let last = parse_day(text(d, &["end_day"])).unwrap_or(open_end);
        return first <= end && last >= start;
    }
    (start..=end).contains(&first)
}

/// The documents of `collection` that belong to `start..=end`, oldest first.
pub fn select(collection: &str, docs: impl IntoIterator<Item = Value>, start: NaiveDate, end: NaiveDate) -> Vec<Value> {
    let mut out: Vec<Value> = docs.into_iter().filter(|d| in_range(collection, d, start, end)).collect();
    sort_docs(&mut out);
    out
}

/// Chronological order: by day, then by the document's own start time.
pub fn sort_docs(docs: &mut [Value]) {
    let key = |d: &Value| {
        let when = ["start_datetime", "bedtime_start", "start_time", "timestamp"]
            .iter()
            .find_map(|k| text(d, &[k]).and_then(parse_timestamp))
            .map(|t| t.with_timezone(&chrono::Utc));
        (doc_day(d), when)
    };
    docs.sort_by_key(key);
}

// ---- rendering ----

pub fn render_table(view: &View, docs: &[Value]) -> String {
    let mut out = format!("{:<12}", "day");
    for c in &view.columns {
        out += &format!("{:<w$}", c.header, w = c.width + 1);
    }
    out = out.trim_end().to_string() + "\n";
    for d in docs {
        let mut line = format!("{:<12}", doc_day(d).map(|d| d.to_string()).unwrap_or_default());
        for c in &view.columns {
            line += &format!("{:<w$}", (c.cell)(d).unwrap_or_else(|| DASH.into()), w = c.width + 1);
        }
        out += line.trim_end();
        out.push('\n');
    }
    out
}

/// Every field of one document, curated fields first.
pub fn render_detail(first: &[&str], doc: &Value, tz: Tz) -> String {
    let Some(obj) = doc.as_object() else { return String::new() };
    let mut keys: Vec<&String> = obj.keys().filter(|k| k.as_str() != "id").collect();
    keys.sort_by_key(|k| (first.iter().position(|f| f == k).unwrap_or(usize::MAX), (*k).clone()));
    let mut out = String::new();
    for k in keys {
        for line in field_lines(k, k, &obj[k.as_str()], tz, 1) {
            out += &line;
            out.push('\n');
        }
    }
    out
}

const LABEL_WIDTH: usize = 32;

fn field_lines(path: &str, key: &str, v: &Value, tz: Tz, depth: usize) -> Vec<String> {
    let pad = "  ".repeat(depth);
    let label = key.replace('_', " ");
    let width = LABEL_WIDTH.saturating_sub(pad.len());
    let line = |value: String| format!("{pad}{label:<width$}{value}");
    match v {
        Value::Null => vec![line(DASH.into())],
        Value::Bool(b) => vec![line(if *b { "yes" } else { "no" }.into())],
        Value::Number(n) => vec![line(format_number(path, key, n.as_f64().unwrap_or(f64::NAN)))],
        Value::String(s) if key.contains("phase") || key == "movement_30_sec" => {
            vec![line(format!("{} coded epochs; decoded by `somno stages`", s.chars().count()))]
        }
        Value::String(s) => vec![line(format_string(s, tz))],
        Value::Object(m) if is_sample(m) => vec![line(sample_summary(key, v))],
        Value::Object(_) if key == "optimal_bedtime" => {
            vec![line(bedtime_window(v).map(|w| format!("{w} (local)")).unwrap_or_else(|| DASH.into()))]
        }
        Value::Object(m) => {
            let mut out = vec![format!("{pad}{label}")];
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            for k in keys {
                out.extend(field_lines(&format!("{path}.{k}"), k, &m[k.as_str()], tz, depth + 1));
            }
            out
        }
        Value::Array(a) if a.is_empty() => vec![line("none".into())],
        Value::Array(a) if a.iter().all(Value::is_string) => {
            vec![line(a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))]
        }
        Value::Array(a) => {
            let mut out = vec![line(format!("{} item(s)", a.len()))];
            for item in a {
                let summary = match item.as_object() {
                    Some(m) => m
                        .iter()
                        .map(|(k, v)| match v {
                            Value::String(s) => format!("{k}: {}", format_string(s, tz)),
                            Value::Array(xs) => format!("{k}: {}", xs.iter().map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).collect::<Vec<_>>().join(", ")),
                            other => format!("{k}: {other}"),
                        })
                        .collect::<Vec<_>>()
                        .join(" · "),
                    None => item.to_string(),
                };
                out.push(format!("{pad}  - {summary}"));
            }
            out
        }
    }
}

fn format_number(path: &str, key: &str, x: f64) -> String {
    let is_duration = key.ends_with("_duration")
        || (key.ends_with("_time") && !key.contains("stamp"))
        || matches!(key, "latency" | "stress_high" | "recovery_high" | "time_in_bed");
    if path.starts_with("contributors.") || path.starts_with("readiness.contributors.") {
        return format!("{x:.0} / 100");
    }
    if is_duration {
        return fmt_duration(x).unwrap_or_else(|| x.to_string());
    }
    if key.contains("calories") {
        return format!("{} kcal", thousands(x));
    }
    if key.contains("distance") || key.contains("meters") {
        return distance(x);
    }
    if key.starts_with("temperature") {
        return format!("{x:+.2} °C");
    }
    if key.contains("heart_rate") {
        return format!("{x:.0} bpm");
    }
    if key.contains("hrv") {
        return format!("{x:.0} ms");
    }
    match (path, key) {
        (_, "efficiency") => format!("{x:.0}%"),
        (_, "average_breath") => format!("{x:.1} /min"),
        ("spo2_percentage.average", _) => format!("{x:.1}%"),
        (_, "pulse_wave_velocity") => format!("{x:.1} m/s"),
        (_, "weight") => format!("{x:.1} kg"),
        (_, "height") => format!("{x:.2} m"),
        (_, "steps") => thousands(x),
        _ if x.fract() == 0.0 => format!("{x:.0}"),
        _ => format!("{x:.2}"),
    }
}

fn format_string(s: &str, tz: Tz) -> String {
    if let Some(t) = parse_timestamp(s) {
        let home = t.with_timezone(&tz).offset().fix();
        let local = t.format("%Y-%m-%d %H:%M").to_string();
        return if *t.offset() == home { local } else { format!("{local} ({})", t.format("%:z")) };
    }
    if s.len() > 48 && !s.contains(' ') {
        return format!("(encoded series, {} chars; see --json)", s.len());
    }
    s.replace('_', " ")
}

fn sample_summary(key: &str, v: &Value) -> String {
    let items = sample_items(v);
    let interval = num(v, &["interval"]).map(|i| format!(" every {i:.0}s")).unwrap_or_default();
    let Some(avg) = mean(&items) else { return format!("no samples{interval}") };
    let min = items.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = items.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let unit = if key.contains("heart_rate_variability") || key.contains("hrv") {
        " ms"
    } else if key.contains("heart_rate") {
        " bpm"
    } else {
        ""
    };
    let decimals = if key == "met" { 1 } else { 0 };
    format!("{} samples{interval} · avg {avg:.decimals$}{unit} · min {min:.decimals$} · max {max:.decimals$}", items.len())
}

// ---- heart rate (time series) ----

#[derive(Debug, Default, Clone, PartialEq)]
pub struct HrStats {
    pub samples: usize,
    pub min: f64,
    pub max: f64,
    pub sum: f64,
    pub by_source: BTreeMap<String, usize>,
}

impl HrStats {
    fn add(&mut self, bpm: f64, source: &str) {
        if self.samples == 0 {
            self.min = bpm;
            self.max = bpm;
        }
        self.samples += 1;
        self.min = self.min.min(bpm);
        self.max = self.max.max(bpm);
        self.sum += bpm;
        *self.by_source.entry(source.to_string()).or_default() += 1;
    }

    pub fn avg(&self) -> Option<f64> {
        (self.samples > 0).then(|| self.sum / self.samples as f64)
    }
}

/// A local day, or one local clock hour identified by its start instant (so
/// the two 01:00 hours of a DST fall-back stay separate).
pub type HrKey = (NaiveDate, Option<DateTime<FixedOffset>>);

/// Heart-rate samples bucketed by local day (and hour, when `by_hour`) in
/// `tz`, since samples carry UTC times.
pub fn hr_buckets(samples: &[Value], tz: Tz, by_hour: bool) -> BTreeMap<HrKey, HrStats> {
    let mut out: BTreeMap<HrKey, HrStats> = BTreeMap::new();
    for s in samples {
        let (Some(bpm), Some(t)) = (num(s, &["bpm"]), text(s, &["timestamp"]).and_then(parse_timestamp)) else { continue };
        let local = t.with_timezone(&tz);
        let hour = by_hour.then(|| {
            let since_hour = chrono::Duration::seconds(i64::from(local.minute() * 60 + local.second())) + chrono::Duration::nanoseconds(i64::from(local.nanosecond()));
            (local - since_hour).fixed_offset()
        });
        out.entry((local.date_naive(), hour)).or_default().add(bpm, text(s, &["source"]).unwrap_or("unknown"));
    }
    out
}

pub fn render_hr(buckets: &BTreeMap<HrKey, HrStats>) -> String {
    let by_hour = buckets.keys().any(|(_, h)| h.is_some());
    let mut out = format!("{:<16}{:<9}{:<6}{:<6}{:<6}sources\n", if by_hour { "hour" } else { "day" }, "samples", "min", "avg", "max");
    for ((day, hour), s) in buckets {
        let label = match hour {
            // Show the offset only for an hour that occurs twice.
            Some(h) if buckets.keys().filter(|(_, o)| o.is_some_and(|o| o.format("%H").to_string() == h.format("%H").to_string() && o.date_naive() == h.date_naive())).count() > 1 => {
                format!("{} ({})", h.format("%H:00"), h.format("%:z"))
            }
            Some(h) => h.format("%H:00").to_string(),
            None => day.to_string(),
        };
        let sources: Vec<String> = s.by_source.iter().map(|(k, n)| format!("{k} {n}")).collect();
        out += &format!(
            "{label:<16}{:<9}{:<6.0}{:<6.0}{:<6.0}{}\n",
            s.samples,
            s.min,
            s.avg().unwrap_or(f64::NAN),
            s.max,
            sources.join(", ")
        );
    }
    out
}

// ---- one-day dashboard ----

fn first_on<'a>(docs: &'a BTreeMap<String, Vec<Value>>, collection: &str) -> Option<&'a Value> {
    docs.get(collection).and_then(|d| d.first())
}

/// Everything Oura has for one day, one line per data type.
pub fn render_day(day: NaiveDate, tz: Tz, docs: &BTreeMap<String, Vec<Value>>, errors: &BTreeMap<String, String>) -> String {
    let mut out = format!("{} ({})\n", day.format("%a %Y-%m-%d"), tz.name());
    // A section whose collection failed says so instead of "none", "off" or
    // a partial answer; the reasons follow at the end.
    let failed = |collections: &[&str]| collections.iter().any(|c| errors.contains_key(*c));
    let mut line = |label: &str, collections: &[&str], value: Option<String>| {
        let value = if failed(collections) { "unavailable (see below)".to_string() } else { value.unwrap_or_else(|| DASH.into()) };
        out += &format!("{label:<14}{value}\n");
    };

    line("readiness", &["daily_readiness"], first_on(docs, "daily_readiness").map(|d| {
        let mut s = f0(num(d, &["score"])).unwrap_or_else(|| DASH.into());
        if let Some(t) = num(d, &["temperature_deviation"]) {
            s += &format!(" · temp {t:+.2} °C");
        }
        s
    }));
    let periods: Vec<&Value> = docs.get("sleep").map(|v| v.iter().filter(|p| crate::oura::is_confirmed_sleep(p)).collect()).unwrap_or_default();
    let main = crate::oura::main_period(&periods);
    line("sleep", &["sleep", "daily_sleep"], {
        let score = first_on(docs, "daily_sleep").and_then(|d| f0(num(d, &["score"])));
        let main_text = main.map(|p| {
            format!(
                "{}, {} asleep (deep {}, REM {})",
                clock_span(p, "bedtime_start", "bedtime_end").unwrap_or_else(|| DASH.into()),
                dur(num(p, &["total_sleep_duration"])).unwrap_or_else(|| DASH.into()),
                dur(num(p, &["deep_sleep_duration"])).unwrap_or_else(|| DASH.into()),
                dur(num(p, &["rem_sleep_duration"])).unwrap_or_else(|| DASH.into()),
            )
        });
        let others = periods.len().saturating_sub(usize::from(main.is_some()));
        match (score, main_text) {
            (None, None) => None,
            (s, m) => Some(format!(
                "{}{}{}",
                s.unwrap_or_else(|| DASH.into()),
                m.map(|m| format!(" · {m}")).unwrap_or_default(),
                if others > 0 { format!(" · +{others} other period(s)") } else { String::new() }
            )),
        }
    });
    line("activity", &["daily_activity"], first_on(docs, "daily_activity").map(|d| {
        format!(
            "{} · {} steps · {} active / {} total kcal",
            f0(num(d, &["score"])).unwrap_or_else(|| DASH.into()),
            num(d, &["steps"]).map(thousands).unwrap_or_else(|| DASH.into()),
            num(d, &["active_calories"]).map(thousands).unwrap_or_else(|| DASH.into()),
            num(d, &["total_calories"]).map(thousands).unwrap_or_else(|| DASH.into()),
        )
    }));
    line("stress", &["daily_stress"], first_on(docs, "daily_stress").map(|d| {
        format!(
            "{} · high stress {} · high recovery {}",
            text(d, &["day_summary"]).unwrap_or(DASH),
            dur(num(d, &["stress_high"])).unwrap_or_else(|| DASH.into()),
            dur(num(d, &["recovery_high"])).unwrap_or_else(|| DASH.into()),
        )
    }));
    line("resilience", &["daily_resilience"], first_on(docs, "daily_resilience").and_then(|d| text(d, &["level"]).map(str::to_string)));
    line("SpO2", &["daily_spo2"], first_on(docs, "daily_spo2").map(|d| {
        format!(
            "{} · breathing disturbance {}",
            num(d, &["spo2_percentage", "average"]).map(|v| format!("{v:.1}%")).unwrap_or_else(|| DASH.into()),
            f0(num(d, &["breathing_disturbance_index"])).unwrap_or_else(|| DASH.into()),
        )
    }));
    line("cardio age", &["daily_cardiovascular_age"], first_on(docs, "daily_cardiovascular_age").and_then(|d| f0(num(d, &["vascular_age"]))));
    line("VO2 max", &["vO2_max"], first_on(docs, "vO2_max").and_then(|d| f0(num(d, &["vo2_max"]))));
    line("bedtime", &["sleep_time"], first_on(docs, "sleep_time").map(|d| {
        format!(
            "{} · {}",
            d.get("optimal_bedtime").and_then(bedtime_window).unwrap_or_else(|| DASH.into()),
            text(d, &["recommendation"]).or(text(d, &["status"])).unwrap_or(DASH).replace('_', " ")
        )
    }));
    let list = |collection: &str, f: &dyn Fn(&Value) -> String| -> Option<String> {
        let items: Vec<String> = docs.get(collection).map(|v| v.iter().map(f).collect()).unwrap_or_default();
        (!items.is_empty()).then(|| items.join("; "))
    };
    line("workouts", &["workout"], list("workout", &|d| {
        format!(
            "{} {} {}",
            clock_span(d, "start_datetime", "end_datetime").unwrap_or_default(),
            text(d, &["label"]).or(text(d, &["activity"])).unwrap_or("workout").replace('_', " "),
            f0(num(d, &["calories"])).map(|k| format!("{k} kcal")).unwrap_or_default()
        )
        .trim()
        .to_string()
    }).or(Some("none".into())));
    line("sessions", &["session"], list("session", &|d| {
        format!(
            "{} {}",
            clock_span(d, "start_datetime", "end_datetime").unwrap_or_default(),
            text(d, &["type"]).unwrap_or("session")
        )
        .trim()
        .to_string()
    }).or(Some("none".into())));
    let mut tags: Vec<String> = Vec::new();
    for c in ["enhanced_tag", "tag"] {
        for d in docs.get(c).into_iter().flatten() {
            tags.push(tag_name(d).unwrap_or_else(|| "note".into()));
        }
    }
    line("tags", &["enhanced_tag", "tag"], Some(if tags.is_empty() { "none".into() } else { tags.join(", ") }));
    line("rest mode", &["rest_mode_period"], docs.get("rest_mode_period").filter(|v| !v.is_empty()).map(|_| "on".into()).or(Some("off".into())));
    for (collection, error) in errors {
        out += &format!("{collection}: {error}\n");
    }
    out
}

/// Collections the `day` dashboard reads.
pub const DAY_COLLECTIONS: &[&str] = &[
    "daily_readiness", "daily_sleep", "sleep", "daily_activity", "daily_stress", "daily_resilience", "daily_spo2",
    "daily_cardiovascular_age", "vO2_max", "sleep_time", "workout", "session", "enhanced_tag", "tag", "rest_mode_period",
];
