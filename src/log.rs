//! Append-only local log, so a failed run can be diagnosed afterwards by you
//! or an agent (`somno log`). Best-effort: logging never fails a command.
//!
//! What is written: the command and its arguments, each HTTP request's method,
//! host, path and query (sensitive parameters redacted), the status, duration
//! and the provider's short error code, login steps, and the final error.
//! Never written: secrets, passwords, tokens, authorization codes, OAuth state,
//! or response bodies.
//!
//! Default location: `somno.log` in the config directory (0600), rotated to
//! `somno.log.1` past 1 MiB. `SOMNO_LOG=off` disables it; `SOMNO_LOG=<path>`
//! writes elsewhere.

use reqwest::Url;
use std::fmt::Display;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

const MAX_BYTES: u64 = 1024 * 1024;
const REDACTED_PARAMS: &[&str] = &[
    "code", "state", "client_id", "client_secret", "access_token", "refresh_token", "password", "next_token",
    "code_challenge",
];

pub fn path() -> Option<PathBuf> {
    match std::env::var_os("SOMNO_LOG") {
        Some(v) if v == "off" => None,
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => crate::store::config_dir().ok().map(|d| d.join("somno.log")),
    }
}

/// Append one line: `<local time> pid=<pid> <event> <detail>`.
pub fn event(event: &str, detail: impl Display) {
    let Some(path) = path() else { return };
    // Control characters (e.g. a newline in an argument) are replaced so a
    // value can't forge extra log lines.
    let detail: String = detail.to_string().chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let _ = append(&path, &format!(
        "{} pid={} {event} {detail}\n",
        chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false),
        std::process::id()
    ));
}

fn append(path: &PathBuf, line: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() && !dir.exists() {
            fs::create_dir_all(dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
            }
        }
    }
    if fs::metadata(path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let mut rotated = path.clone().into_os_string();
        rotated.push(".1");
        fs::rename(path, rotated)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    // An existing log keeps its old mode on open; make sure it is private.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(line.as_bytes())
}

/// The URL with sensitive query values replaced by `…`.
pub fn redact_url(url: &Url) -> String {
    let mut clean = url.clone();
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| {
            let v = if REDACTED_PARAMS.contains(&k.as_ref()) { "…".to_string() } else { v.into_owned() };
            (k.into_owned(), v)
        })
        .collect();
    clean.set_query(None);
    if !pairs.is_empty() {
        clean.query_pairs_mut().extend_pairs(pairs);
    }
    clean.set_username("").ok();
    clean.set_password(None).ok();
    clean.to_string()
}

/// The last `n` lines of the log.
pub fn tail(n: usize) -> std::io::Result<Vec<String>> {
    let Some(path) = path() else { return Ok(vec![]) };
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    Ok(lines[lines.len().saturating_sub(n)..].to_vec())
}
