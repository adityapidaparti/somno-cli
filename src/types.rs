//! Pure helpers: day specs, durations, timestamps. No I/O.

use chrono::{DateTime, Duration, FixedOffset, NaiveDate};
use std::fmt;
use std::str::FromStr;

/// A civil day relative to "today" in the display timezone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaySpec {
    Today,
    Offset(i64),
    Date(NaiveDate),
}

impl DaySpec {
    /// Resolve against a reference "today" (injected for testability).
    pub fn resolve(&self, today: NaiveDate) -> NaiveDate {
        match self {
            DaySpec::Today => today,
            DaySpec::Offset(n) => today + Duration::days(*n),
            DaySpec::Date(d) => *d,
        }
    }
}

impl FromStr for DaySpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim().to_ascii_lowercase();
        match s.as_str() {
            "today" | "" => return Ok(DaySpec::Today),
            "yesterday" => return Ok(DaySpec::Offset(-1)),
            _ => {}
        }
        // "-N": digits only after a single minus, bounded, so "--1" or a huge
        // number is a usage error rather than a wrong day or an overflow.
        if let Some(rest) = s.strip_prefix('-') {
            if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                return match rest.parse::<i64>() {
                    Ok(n) if n <= 100_000 => Ok(DaySpec::Offset(-n)),
                    _ => Err(format!("'{s}' is too far back (at most -100000)")),
                };
            }
        }
        NaiveDate::parse_from_str(&s, "%Y-%m-%d")
            .map(DaySpec::Date)
            .map_err(|_| format!("unknown day '{s}' (expected today|yesterday|-N|YYYY-MM-DD)"))
    }
}

impl fmt::Display for DaySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DaySpec::Today | DaySpec::Offset(0) => f.write_str("today"),
            DaySpec::Offset(-1) => f.write_str("yesterday"),
            DaySpec::Offset(n) if *n < 0 => write!(f, "-{}", -n),
            DaySpec::Offset(n) => write!(f, "+{n}"),
            DaySpec::Date(d) => write!(f, "{}", d.format("%Y-%m-%d")),
        }
    }
}

/// `6h 52m`, `39m`. Negative or non-finite input is unknown, not zero.
pub fn fmt_duration(secs: f64) -> Option<String> {
    if !(0.0..=1e9).contains(&secs) {
        return None;
    }
    let mins = (secs / 60.0).round() as i64;
    Some(if mins >= 60 {
        format!("{}h {:02}m", mins / 60, mins % 60)
    } else {
        format!("{mins}m")
    })
}

/// RFC 3339 as both providers send it (`…-04:00`, `…Z`, with or without a fraction).
pub fn parse_timestamp(s: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(s.trim()).ok()
}

/// `start..=end`; empty when `end < start`.
pub fn days_inclusive(start: NaiveDate, end: NaiveDate) -> Vec<NaiveDate> {
    start.iter_days().take_while(|d| *d <= end).collect()
}
