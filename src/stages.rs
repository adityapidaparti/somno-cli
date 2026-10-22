//! Sleep stages within an Oura sleep period, decoded from its phase strings.
//!
//! Oura OpenAPI 1.42: `sleep_phase_30_sec` / `sleep_phase_5_min` hold one
//! character per epoch from `bedtime_start`: '1' deep, '2' light, '3' REM,
//! '4' awake. `movement_30_sec`: '1' no motion, '2' restless, '3' tossing and
//! turning, '4' active. The 30-second string is preferred: its counts
//! reproduce Oura's deep/light/REM durations exactly (checked on real data
//! 2026-10-21; awake differs by under one epoch because the last is partial).
//!
//! Everything here is counted from those strings. "Stretches", onset and
//! final wake are derived, not Oura fields.

use chrono::{DateTime, Duration, FixedOffset};
use serde::Serialize;
use serde_json::{json, Value};

use crate::night::text;
use crate::types::{fmt_duration, parse_timestamp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Deep,
    Light,
    Rem,
    Awake,
}

impl Stage {
    pub const ALL: [Stage; 4] = [Stage::Deep, Stage::Light, Stage::Rem, Stage::Awake];

    pub fn from_code(c: char) -> Option<Stage> {
        match c {
            '1' => Some(Stage::Deep),
            '2' => Some(Stage::Light),
            '3' => Some(Stage::Rem),
            '4' => Some(Stage::Awake),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Stage::Deep => "deep",
            Stage::Light => "light",
            Stage::Rem => "REM",
            Stage::Awake => "awake",
        }
    }

    fn asleep(self) -> bool {
        self != Stage::Awake
    }
}

/// Epochs of one sleep period. `None` marks a character Oura didn't define.
#[derive(Debug, Clone, PartialEq)]
pub struct Hypnogram {
    pub start: DateTime<FixedOffset>,
    /// Oura's `bedtime_end`, when it parses.
    pub end: Option<DateTime<FixedOffset>>,
    pub epoch_secs: i64,
    pub epochs: Vec<Option<Stage>>,
}

impl Hypnogram {
    /// Finest available series: 30-second, then 5-minute, then the app's 5-minute.
    pub fn from_doc(doc: &Value) -> Option<Hypnogram> {
        let start = parse_timestamp(text(doc, &["bedtime_start"])?)?;
        let (codes, epoch_secs) = [("sleep_phase_30_sec", 30), ("sleep_phase_5_min", 300), ("app_sleep_phase_5_min", 300)]
            .iter()
            .find_map(|(k, secs)| text(doc, &[k]).map(|s| (s, *secs)))?;
        let end = text(doc, &["bedtime_end"]).and_then(parse_timestamp);
        Some(Hypnogram { start, end, epoch_secs, epochs: codes.chars().map(Stage::from_code).collect() })
    }

    pub fn at(&self, epoch: usize) -> DateTime<FixedOffset> {
        self.start + Duration::seconds(self.epoch_secs * epoch as i64)
    }

    pub fn runs(&self) -> Vec<Run> {
        let mut out: Vec<Run> = Vec::new();
        for (i, s) in self.epochs.iter().enumerate() {
            match out.last_mut() {
                Some(r) if r.stage == *s => r.len += 1,
                _ => out.push(Run { start: i, len: 1, stage: *s }),
            }
        }
        out
    }

    /// First and last asleep epochs.
    pub fn sleep_window(&self) -> Option<(usize, usize)> {
        let first = self.epochs.iter().position(|s| s.is_some_and(Stage::asleep))?;
        let last = self.epochs.iter().rposition(|s| s.is_some_and(Stage::asleep))?;
        Some((first, last))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Run {
    pub start: usize,
    pub len: usize,
    pub stage: Option<Stage>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StageStats {
    pub seconds: i64,
    pub longest_seconds: i64,
    /// Uninterrupted stretches of at least five minutes.
    pub stretches_5min: usize,
}

pub fn stage_stats(h: &Hypnogram) -> Vec<(Stage, StageStats)> {
    let runs = h.runs();
    Stage::ALL
        .iter()
        .map(|&stage| {
            let mine: Vec<&Run> = runs.iter().filter(|r| r.stage == Some(stage)).collect();
            let secs = |r: &&Run| r.len as i64 * h.epoch_secs;
            (
                stage,
                StageStats {
                    seconds: mine.iter().map(secs).sum(),
                    longest_seconds: mine.iter().map(secs).max().unwrap_or(0),
                    stretches_5min: mine.iter().filter(|r| secs(r) >= 300).count(),
                },
            )
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Events {
    /// Oura's sleep onset (`bedtime_start + latency`) when it lies in the
    /// period and no later than the last sleep; otherwise the first sleep epoch.
    pub fell_asleep: Option<DateTime<FixedOffset>>,
    /// First epoch scored as sleep. Can be earlier than Oura's onset, which
    /// does not count brief dozing.
    pub first_sleep_epoch: Option<DateTime<FixedOffset>>,
    pub woke_for_good: Option<DateTime<FixedOffset>>,
    pub first_deep: Option<DateTime<FixedOffset>>,
    pub first_rem: Option<DateTime<FixedOffset>>,
    /// Awake time from `fell_asleep` to the end of the last epoch scored as
    /// sleep, in seconds (not rounded to epochs).
    pub awake_between_seconds: i64,
    /// Awake stretches of at least five minutes in that window (a stretch
    /// cut by the window counts only its part inside).
    pub awakenings_5min: usize,
}

pub fn events(h: &Hypnogram, latency_secs: Option<f64>) -> Events {
    let window = h.sleep_window();
    let first_sleep_epoch = window.map(|(a, _)| h.at(a));
    let epoch = h.epoch_secs.max(1);
    // Seconds from the start of the period to the end of the last sleep epoch.
    let last_sleep_end = window.map(|(_, b)| (b as i64 + 1) * epoch);
    // A latency outside the period (negative, non-finite, past the end of the
    // codes or of Oura's bedtime_end) or after the last sleep is not a usable
    // onset; fall back to the first sleep epoch.
    let mut span = h.epochs.len() as f64 * epoch as f64;
    if let Some(secs) = h.end.map(|e| (e - h.start).num_seconds()).filter(|s| *s >= 0) {
        span = span.min(secs as f64);
    }
    let latency_secs = latency_secs
        .filter(|l| l.is_finite() && *l >= 0.0 && *l <= span)
        .map(|l| l as i64)
        .filter(|l| last_sleep_end.is_none_or(|end| *l <= end));
    let oura_onset = latency_secs.map(|l| h.start + Duration::seconds(l));
    let first = |stage: Stage| h.epochs.iter().position(|s| *s == Some(stage)).map(|i| h.at(i));
    // Wake is counted in seconds from the same onset "fell asleep" shows
    // (Oura's, when given) to the end of the last epoch scored as sleep, so
    // 5-minute and 30-second codes agree. Awake time before that onset is
    // falling asleep, not waking up.
    let (awake_between_seconds, awakenings_5min) = match (window, last_sleep_end) {
        (Some((a, _)), Some(to)) => {
            let from = latency_secs.unwrap_or(a as i64 * epoch);
            let mut total = 0i64;
            let mut long = 0usize;
            for r in h.runs().into_iter().filter(|r| r.stage == Some(Stage::Awake)) {
                let (lo, hi) = ((r.start as i64 * epoch).max(from), ((r.start + r.len) as i64 * epoch).min(to));
                if hi > lo {
                    total += hi - lo;
                    long += usize::from(hi - lo >= 300);
                }
            }
            (total, long)
        }
        _ => (0, 0),
    };
    Events {
        fell_asleep: oura_onset.or(first_sleep_epoch),
        first_sleep_epoch,
        woke_for_good: window.map(|(_, b)| h.at(b + 1)),
        first_deep: first(Stage::Deep),
        first_rem: first(Stage::Rem),
        awake_between_seconds,
        awakenings_5min,
    }
}

/// Seconds per movement class from `movement_30_sec`.
pub fn movement(doc: &Value) -> Option<Vec<(&'static str, i64)>> {
    let codes = text(doc, &["movement_30_sec"])?;
    let count = |c: char| codes.chars().filter(|x| *x == c).count() as i64 * 30;
    Some(vec![("still", count('1')), ("restless", count('2')), ("tossing and turning", count('3')), ("active", count('4'))])
}

// ---- rendering ----

fn hm(t: &DateTime<FixedOffset>) -> String {
    t.format("%H:%M").to_string()
}

fn hms(t: &DateTime<FixedOffset>) -> String {
    t.format("%H:%M:%S").to_string()
}

/// Durations at epoch precision: `30s`, `4m 30s`, `1h 05m`, `1h 00m 30s`.
fn exact(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s % 60 == 0 => d(s),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m {:02}s", s / 3600, s / 60 % 60, s % 60),
    }
}

fn d(secs: i64) -> String {
    fmt_duration(secs as f64).unwrap_or_default()
}

fn pct(part: i64, whole: i64) -> String {
    if whole > 0 { format!("{:.0}%", 100.0 * part as f64 / whole as f64) } else { "—".into() }
}

/// Four-row hypnogram, awake on top, at most `width` columns. Each column
/// shows the most common stage in its bin.
pub fn render_chart(h: &Hypnogram, width: usize) -> String {
    if h.epochs.is_empty() {
        return String::new();
    }
    let per_col = h.epochs.len().div_ceil(width.max(1));
    let cols: Vec<Option<Stage>> = h
        .epochs
        .chunks(per_col)
        .map(|bin| {
            [Stage::Awake, Stage::Rem, Stage::Light, Stage::Deep]
                .into_iter()
                .map(|s| (bin.iter().filter(|x| **x == Some(s)).count(), s))
                .filter(|(n, _)| *n > 0)
                .max_by_key(|(n, _)| *n)
                .map(|(_, s)| s)
        })
        .collect();
    let mut out = String::new();
    for row in [Stage::Awake, Stage::Rem, Stage::Light, Stage::Deep] {
        let line: String = cols.iter().map(|c| if *c == Some(row) { '█' } else { ' ' }).collect();
        out += &format!("  {:<6}{}\n", row.label(), line.trim_end());
    }
    // Hour marks under the chart.
    let mut axis = vec![' '; cols.len() + 6];
    let mut last_label_end = 0usize;
    for c in 0..cols.len() {
        let t = h.at(c * per_col);
        let prev = if c == 0 { None } else { Some(h.at((c - 1) * per_col)) };
        let new_hour = c == 0 || prev.is_some_and(|p| p.format("%H").to_string() != t.format("%H").to_string());
        if new_hour && (c == 0 || c > last_label_end) {
            let label = if c == 0 { hm(&t) } else { t.format("%H:00").to_string() };
            if c + label.len() <= axis.len() {
                for (i, ch) in label.chars().enumerate() {
                    axis[c + i] = ch;
                }
                last_label_end = c + label.len();
            }
        }
    }
    out += &format!("        {}\n", axis.into_iter().collect::<String>().trim_end());
    out += &format!("        one column ≈ {}\n", d(per_col as i64 * h.epoch_secs));
    out
}

/// Everything about the stages of one sleep period.
pub fn render_period(doc: &Value, width: usize, segments: bool) -> String {
    let kind = text(doc, &["type"]).unwrap_or("sleep").replace('_', " ");
    let Some(h) = Hypnogram::from_doc(doc) else {
        return format!("{kind}: no stage data in this period\n");
    };
    // All times stay in the offset the period started in; if the phone's
    // timezone changed during the night, the local end time is noted.
    let end_local = text(doc, &["bedtime_end"]).and_then(parse_timestamp).unwrap_or_else(|| h.at(h.epochs.len()));
    let end = end_local.with_timezone(h.start.offset());
    let offset = h.start.format("%:z");
    let mut out = format!("{kind} · {} → {} ({offset}) · {}-second stages\n", hm(&h.start), hm(&end), h.epoch_secs);
    if end_local.offset() != h.start.offset() {
        out += &format!("timezone changed during this period: it ended at {} local ({})\n", hm(&end_local), end_local.format("%:z"));
    }
    out.push('\n');
    out += &render_chart(&h, width);

    let stats = stage_stats(&h);
    // Time in bed includes epochs with codes Oura didn't define.
    let in_bed = h.epochs.len() as i64 * h.epoch_secs;
    let asleep: i64 = stats.iter().filter(|(s, _)| s.asleep()).map(|(_, s)| s.seconds).sum();
    out += &format!("\n  {:<8}{:<9}{:<10}{:<12}{:<9}{}\n", "stage", "time", "of bed", "of sleep", "longest", "stretches ≥5m");
    for (stage, s) in &stats {
        out += &format!(
            "  {:<8}{:<9}{:<10}{:<12}{:<9}{}\n",
            stage.label(),
            d(s.seconds),
            pct(s.seconds, in_bed),
            if stage.asleep() { pct(s.seconds, asleep) } else { String::new() },
            d(s.longest_seconds),
            s.stretches_5min
        );
    }

    let e = events(&h, crate::night::num(doc, &["latency"]));
    let since = |t: Option<DateTime<FixedOffset>>, from: Option<DateTime<FixedOffset>>, what: &str| match (t, from) {
        (Some(t), Some(f)) if t < f => format!("{} (before {what})", hm(&t)),
        (Some(t), Some(f)) => format!("{} ({} after {what})", hm(&t), d((t - f).num_seconds())),
        (Some(t), None) => hm(&t),
        _ => "—".into(),
    };
    out += "\n";
    out += &format!("  {:<18}{}\n", "fell asleep", since(e.fell_asleep, Some(h.start), "bedtime"));
    if let (Some(first), Some(onset)) = (e.first_sleep_epoch, e.fell_asleep) {
        if first < onset {
            out += &format!("  {:<18}{} (brief sleep before Oura's onset)\n", "first dozed off", hm(&first));
        }
    }
    out += &format!("  {:<18}{}\n", "first deep", since(e.first_deep, e.fell_asleep, "falling asleep"));
    out += &format!("  {:<18}{}\n", "first REM", since(e.first_rem, e.fell_asleep, "falling asleep"));
    out += &format!(
        "  {:<18}{} in between, {} stretch(es) ≥5m\n",
        "awake",
        d(e.awake_between_seconds),
        e.awakenings_5min
    );
    out += &format!("  {:<18}{}\n", "woke for good", e.woke_for_good.map(|t| hm(&t)).unwrap_or_else(|| "—".into()));
    if let Some(m) = movement(doc) {
        let total: i64 = m.iter().map(|(_, s)| s).sum();
        let parts: Vec<String> = m.iter().map(|(name, s)| format!("{name} {}", pct(*s, total))).collect();
        out += &format!("  {:<18}{}\n", "movement", parts.join(" · "));
    }
    if segments {
        out += "\n  segments\n";
        for r in h.runs() {
            out += &format!(
                "  {}–{}  {:<6} {}\n",
                hms(&h.at(r.start)),
                hms(&h.at(r.start + r.len)),
                r.stage.map(Stage::label).unwrap_or("?"),
                exact(r.len as i64 * h.epoch_secs)
            );
        }
    }
    out
}

/// Machine-readable stages for one period: totals, events, movement and every
/// run of identical epochs.
pub fn period_json(doc: &Value) -> Value {
    let Some(h) = Hypnogram::from_doc(doc) else {
        return json!({"id": text(doc, &["id"]), "type": text(doc, &["type"]), "stages": null});
    };
    let stats: serde_json::Map<String, Value> = stage_stats(&h)
        .into_iter()
        .map(|(s, st)| (format!("{s:?}").to_lowercase(), serde_json::to_value(st).unwrap_or(Value::Null)))
        .collect();
    let runs: Vec<Value> = h
        .runs()
        .into_iter()
        .map(|r| json!({
            "start": h.at(r.start).to_rfc3339(),
            "end": h.at(r.start + r.len).to_rfc3339(),
            "stage": r.stage,
            "seconds": r.len as i64 * h.epoch_secs,
        }))
        .collect();
    json!({
        "id": text(doc, &["id"]),
        "type": text(doc, &["type"]),
        "bedtime_start": text(doc, &["bedtime_start"]),
        "bedtime_end": text(doc, &["bedtime_end"]),
        "epoch_seconds": h.epoch_secs,
        "totals": stats,
        "events": events(&h, crate::night::num(doc, &["latency"])),
        "movement_seconds": movement(doc).map(|m| m.into_iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<String, Value>>()),
        "segments": runs,
    })
}
