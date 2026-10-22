use anyhow::{anyhow, bail, Context, Result};
use chrono::{Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::{json, Value};
use std::io::Write;

use somno::eight::{self, Credentials, Eight};
use somno::night::{self, Source, SourceNights};
use somno::oura::{self, ClientConfig, Oura};
use somno::types::{days_inclusive, fmt_duration, DaySpec};
use somno::{http, log, store, views};
use std::collections::BTreeMap;

#[derive(Parser)]
#[command(name = "somno", about = "Read Oura and Eight Sleep sleep data from the terminal (read-only)", version)]
struct Cli {
    /// IANA timezone for "today" and displayed times [default: $SOMNO_TZ, else the system zone]
    #[arg(long, global = true)]
    tz: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// One night, both sources side by side: somno night yesterday
    Night {
        /// Day the night ends on: today|yesterday|-N|YYYY-MM-DD
        #[arg(default_value = "today", allow_hyphen_values = true)]
        day: DaySpec,
        #[command(flatten)]
        view: View,
    },
    /// The seven nights ending on END
    Week {
        #[arg(default_value = "today", allow_hyphen_values = true)]
        end: DaySpec,
        #[command(flatten)]
        view: View,
    },
    /// Every night from START to END, inclusive
    Range {
        #[arg(allow_hyphen_values = true)]
        start: DaySpec,
        #[arg(default_value = "today", allow_hyphen_values = true)]
        end: DaySpec,
        #[command(flatten)]
        view: View,
    },
    /// Everything Oura has for one day on one screen
    Day {
        #[arg(default_value = "today", allow_hyphen_values = true)]
        day: DaySpec,
        /// Oura's original documents for the day, by collection
        #[arg(long)]
        json: bool,
    },
    /// Readiness score, temperature deviation and contributors
    Readiness(RangeArgs),
    /// Every Oura sleep period, naps included (`night` shows only the main one)
    Sleep(RangeArgs),
    /// Sleep stages through each sleep period of a day: hypnogram, stage times, onset, wake-ups, movement
    Stages {
        #[arg(default_value = "today", allow_hyphen_values = true)]
        day: DaySpec,
        /// Also list every stage change
        #[arg(long)]
        segments: bool,
        /// Chart width in columns
        #[arg(long, default_value_t = 96)]
        width: usize,
        /// Decoded stages as JSON (totals, events, movement, every segment)
        #[arg(long)]
        json: bool,
    },
    /// Daily sleep score and its contributors
    SleepScore(RangeArgs),
    /// Steps, calories, activity time and score
    Activity(RangeArgs),
    /// Daytime stress and recovery
    Stress(RangeArgs),
    /// Resilience level and contributors
    Resilience(RangeArgs),
    /// Overnight blood oxygen and breathing disturbance index
    Spo2(RangeArgs),
    /// Estimated vascular age and pulse wave velocity
    CardioAge(RangeArgs),
    /// VO2 max estimates
    Vo2max(RangeArgs),
    /// Detected and logged workouts
    Workouts(RangeArgs),
    /// Breathing, meditation, rest and other sessions
    Sessions(RangeArgs),
    /// Tags and notes you entered in the Oura app
    Tags(RangeArgs),
    /// Oura's recommended bedtime window
    Bedtime(RangeArgs),
    /// Rest mode periods
    RestMode(RangeArgs),
    /// Heart rate: min/avg/max per day, or per hour for a single day (max 31 days)
    Hr(RangeArgs),
    /// Your Oura profile: age, sex, height, weight, email
    Me {
        #[arg(long)]
        json: bool,
    },
    /// Local configuration and token state (no network)
    Status,
    /// Oura setup
    #[command(subcommand)]
    Oura(OuraCmd),
    /// Eight Sleep setup
    #[command(subcommand)]
    Eight(EightCmd),
    /// Original provider JSON, unmodified
    #[command(subcommand)]
    Raw(RawCmd),
    /// Recent log lines: commands, HTTP requests (no secrets or bodies), login steps, errors
    Log {
        #[arg(short = 'n', long, default_value_t = 40)]
        lines: usize,
    },
    /// Print the agent skill for this version, or install it with `skill install`
    Skill {
        #[command(subcommand)]
        action: Option<SkillCmd>,
    },
}

#[derive(Subcommand)]
enum SkillCmd {
    /// Write the skill to DIR/NAME/SKILL.md [default DIR: ~/.claude/skills, read by Claude Code and Grok CLI]
    Install {
        /// Skills directory of another agent
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
    },
}

/// No dates: the last 7 days as a table. One day: every field for that day.
/// Two days: that range as a table.
#[derive(Args)]
struct RangeArgs {
    /// A day, or the first day of a range: today|yesterday|-N|YYYY-MM-DD
    #[arg(allow_hyphen_values = true)]
    start: Option<DaySpec>,
    /// Last day of the range
    #[arg(allow_hyphen_values = true)]
    end: Option<DaySpec>,
    /// Oura's original documents for these days, by collection
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct View {
    /// Normalized JSON instead of a table
    #[arg(long)]
    json: bool,
    /// Query only one source
    #[arg(long, value_enum)]
    only: Option<Only>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Only {
    Oura,
    Eight,
}

#[derive(Subcommand)]
enum OuraCmd {
    /// Save your Oura developer app's client ID and secret (prompts), after checking them with Oura
    Configure,
    /// Authorize in a browser: caught on localhost:8765, or paste the redirect URL (SSH, servers)
    Login,
    /// After login: try every Oura data type and show which ones your granted scopes reach
    Check,
    /// Delete saved Oura tokens (keeps the client config)
    Logout,
}

#[derive(Subcommand)]
enum EightCmd {
    /// Log in with your Eight Sleep account (prompts for the password)
    Login {
        email: String,
        /// Remember the password on disk (0600) so expired sessions renew automatically
        #[arg(long)]
        remember: bool,
    },
    /// Delete the saved Eight Sleep session and any remembered password
    Logout,
}

#[derive(Subcommand)]
enum RawCmd {
    /// Any Oura V2 collection, all pages: somno raw oura sleep --start 2026-10-01 --end 2026-10-08
    Oura {
        collection: String,
        /// Sent as Oura's start_date (start_datetime at local midnight for heartrate/ring_battery_level)
        #[arg(long, allow_hyphen_values = true)]
        start: Option<DaySpec>,
        /// Sent as Oura's end_date (end_datetime at local midnight for time series)
        #[arg(long, allow_hyphen_values = true)]
        end: Option<DaySpec>,
    },
    /// Eight Sleep trends (from/to days) or me (account and devices)
    Eight {
        #[arg(value_enum)]
        endpoint: EightEndpoint,
        #[arg(long, allow_hyphen_values = true)]
        start: Option<DaySpec>,
        #[arg(long, allow_hyphen_values = true)]
        end: Option<DaySpec>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum EightEndpoint {
    Trends,
    Me,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let logged = !matches!(args.first().map(String::as_str), Some("log" | "help" | "--help" | "-h" | "--version" | "-V"));
    if logged {
        log::event("start", format_args!("somno {} (v{})", args.join(" "), env!("CARGO_PKG_VERSION")));
    }
    let result = run();
    if logged {
        match &result {
            Ok(()) => log::event("end", "ok"),
            Err(e) => log::event("end", format_args!("error: {e:#}")),
        }
    }
    result
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let tz = resolve_tz(cli.tz.as_deref())?;
    let today = Utc::now().with_timezone(&tz).date_naive();
    match cli.command {
        Command::Night { day, view } => show(&[day.resolve(today)], tz, &view, true),
        Command::Week { end, view } => {
            let end = end.resolve(today);
            show(&days_inclusive(end - Duration::days(6), end), tz, &view, false)
        }
        Command::Range { start, end, view } => {
            let (start, end) = (start.resolve(today), end.resolve(today));
            if end < start {
                bail!("END ({end}) is before START ({start})");
            }
            if (end - start).num_days() >= 400 {
                bail!("range is limited to 400 nights; split it up");
            }
            show(&days_inclusive(start, end), tz, &view, false)
        }
        Command::Day { day, json } => show_day(day.resolve(today), json, tz),
        Command::Readiness(r) => show_view("readiness", &r, tz, today),
        Command::Sleep(r) => show_view("sleep", &r, tz, today),
        Command::SleepScore(r) => show_view("sleep-score", &r, tz, today),
        Command::Stages { day, segments, width, json } => show_stages(day.resolve(today), segments, width, json),
        Command::Activity(r) => show_view("activity", &r, tz, today),
        Command::Stress(r) => show_view("stress", &r, tz, today),
        Command::Resilience(r) => show_view("resilience", &r, tz, today),
        Command::Spo2(r) => show_view("spo2", &r, tz, today),
        Command::CardioAge(r) => show_view("cardio-age", &r, tz, today),
        Command::Vo2max(r) => show_view("vo2max", &r, tz, today),
        Command::Workouts(r) => show_view("workouts", &r, tz, today),
        Command::Sessions(r) => show_view("sessions", &r, tz, today),
        Command::Tags(r) => show_view("tags", &r, tz, today),
        Command::Bedtime(r) => show_view("bedtime", &r, tz, today),
        Command::RestMode(r) => show_view("rest-mode", &r, tz, today),
        Command::Hr(r) => show_hr(&r, tz, today),
        Command::Me { json } => show_me(json, tz),
        Command::Status => status(tz),
        Command::Oura(cmd) => oura_cmd(cmd, tz, today),
        Command::Eight(cmd) => eight_cmd(cmd),
        Command::Raw(cmd) => raw(cmd, tz, today),
        Command::Skill { action } => skill_cmd(action),
        Command::Log { lines } => {
            match log::path() {
                None => println!("logging is off (SOMNO_LOG=off)"),
                Some(path) => {
                    eprintln!("{}", path.display());
                    for line in log::tail(lines)? {
                        println!("{line}");
                    }
                }
            }
            Ok(())
        }
    }
}

fn skill_cmd(action: Option<SkillCmd>) -> Result<()> {
    use somno::skill;
    match action {
        None => print!("{}", skill::SKILL),
        Some(SkillCmd::Install { dir }) => {
            let dir = match dir {
                Some(d) => d,
                None => skill::default_dir()?,
            };
            println!("{}", skill::install(&dir, skill::NAME, skill::SKILL)?);
        }
    }
    Ok(())
}

fn resolve_tz(flag: Option<&str>) -> Result<Tz> {
    let name = match flag {
        Some(n) => n.to_string(),
        None => match std::env::var("SOMNO_TZ") {
            Ok(n) if !n.is_empty() => n,
            _ => iana_time_zone::get_timezone().context("cannot detect the system timezone; pass --tz ZONE")?,
        },
    };
    name.parse::<Tz>().map_err(|_| anyhow!("unknown IANA timezone '{name}'"))
}

fn fetch_oura(days: &[NaiveDate]) -> (SourceNights, Vec<String>) {
    let result = http::client().and_then(Oura::load).and_then(|o| o.nights(days));
    match result {
        Ok((nights, notes)) => (Ok(nights), notes),
        Err(e) => (Err(format!("{e:#}")), vec![]),
    }
}

fn fetch_eight(days: &[NaiveDate], tz: Tz) -> SourceNights {
    http::client()
        .and_then(Eight::load)
        .and_then(|mut e| e.nights(days, tz.name()))
        .map_err(|e| format!("{e:#}"))
}

fn show(days: &[NaiveDate], tz: Tz, view: &View, single: bool) -> Result<()> {
    let want_oura = !matches!(view.only, Some(Only::Eight));
    let want_eight = !matches!(view.only, Some(Only::Oura));
    let ((oura_nights, notes), eight_nights) = std::thread::scope(|s| {
        let o = want_oura.then(|| s.spawn(|| fetch_oura(days)));
        let e = want_eight.then(|| s.spawn(|| fetch_eight(days, tz)));
        let panicked = || "internal error: fetch thread panicked".to_string();
        (
            o.map(|h| h.join().unwrap_or_else(|_| (Err(panicked()), vec![]))).unwrap_or((Err(String::new()), vec![])),
            e.map(|h| h.join().unwrap_or_else(|_| Err(panicked()))),
        )
    });
    let mut sources: Vec<(Source, SourceNights)> = Vec::new();
    if want_oura {
        sources.push((Source::Oura, oura_nights));
    }
    if let Some(e) = eight_nights {
        sources.push((Source::EightSleep, e));
    }

    if view.json {
        let out = json!({
            "timezone": tz.name(),
            "days": days.iter().map(|day| {
                let mut entry = json!({"day": day.to_string()});
                for (source, nights) in &sources {
                    entry[source.key()] = serde_json::to_value(night::slot(nights, *day)).unwrap_or(Value::Null);
                }
                entry
            }).collect::<Vec<_>>(),
            "notes": notes,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else if single {
        let columns: Vec<(Source, night::Slot)> = sources.iter().map(|(s, n)| (*s, night::slot(n, days[0]))).collect();
        print!("{}", night::render_night(days[0], tz, &columns));
    } else {
        print!("{}", night::render_table(days, tz, &sources));
    }
    for note in &notes {
        eprintln!("note: {note}");
    }
    if sources.iter().all(|(_, n)| n.is_err()) {
        bail!("no source could be read");
    }
    Ok(())
}

/// (start, end, single day?)
fn resolve_range(r: &RangeArgs, today: NaiveDate, max_days: i64) -> Result<(NaiveDate, NaiveDate, bool)> {
    let (start, end, single) = match (r.start, r.end) {
        (Some(s), Some(e)) => (s.resolve(today), e.resolve(today), false),
        (Some(s), None) => {
            let d = s.resolve(today);
            (d, d, true)
        }
        _ => (today - Duration::days(6), today, false),
    };
    if end < start {
        bail!("END ({end}) is before START ({start})");
    }
    if (end - start).num_days() >= max_days {
        bail!("limited to {max_days} days per query; split the range");
    }
    Ok((start, end, single))
}

fn json_out(tz: Tz, start: NaiveDate, end: NaiveDate, collections: &BTreeMap<String, Vec<Value>>, errors: &BTreeMap<String, String>) -> Result<()> {
    let out = json!({
        "timezone": tz.name(),
        "start": start.to_string(),
        "end": end.to_string(),
        "collections": collections,
        "errors": errors,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn show_view(name: &str, r: &RangeArgs, tz: Tz, today: NaiveDate) -> Result<()> {
    let view = views::view(name).expect("every command maps to a view");
    let (start, end, single) = resolve_range(r, today, 400)?;
    let oura = Oura::load(http::client()?)?;
    let mut collections = BTreeMap::new();
    let mut errors = BTreeMap::new();
    for c in view.collections {
        match oura.docs_in(c, start, end) {
            Ok(docs) => {
                collections.insert(c.to_string(), docs);
            }
            Err(e) => {
                errors.insert(c.to_string(), format!("{e:#}"));
            }
        }
    }
    if collections.is_empty() {
        bail!("{}", errors.values().cloned().collect::<Vec<_>>().join("; "));
    }
    if r.json {
        return json_out(tz, start, end, &collections, &errors);
    }
    let mut docs: Vec<Value> = collections.into_values().flatten().collect();
    views::sort_docs(&mut docs);
    if single {
        println!("{} · {}", view.title, start.format("%a %Y-%m-%d"));
        if docs.is_empty() {
            println!("nothing recorded");
        }
        for (i, d) in docs.iter().enumerate() {
            if i > 0 {
                println!();
            }
            print!("{}", views::render_detail(view.first, d, tz));
        }
    } else {
        println!("{} · {start} → {end}", view.title);
        if docs.is_empty() {
            println!("nothing recorded");
        } else {
            print!("{}", views::render_table(&view, &docs, tz));
        }
    }
    for (c, e) in &errors {
        eprintln!("note: {c} unavailable: {e}");
    }
    Ok(())
}

fn show_stages(day: NaiveDate, segments: bool, width: usize, json: bool) -> Result<()> {
    let periods: Vec<Value> = Oura::load(http::client()?)?
        .docs_in("sleep", day, day)?
        .into_iter()
        .filter(somno::oura::is_confirmed_sleep)
        .collect();
    if json {
        let out = json!({"day": day.to_string(), "periods": periods.iter().map(somno::stages::period_json).collect::<Vec<_>>()});
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    println!("Sleep stages · {} (Oura)", day.format("%a %Y-%m-%d"));
    if periods.is_empty() {
        println!("no sleep recorded");
    }
    for doc in &periods {
        println!();
        print!("{}", somno::stages::render_period(doc, width.clamp(20, 400), segments));
    }
    Ok(())
}

fn show_hr(r: &RangeArgs, tz: Tz, today: NaiveDate) -> Result<()> {
    let (start, end, single) = resolve_range(r, today, 31)?;
    let params = oura::window_params("heartrate", Some(start), Some(end + Duration::days(1)), tz)?;
    let samples = views::samples_within(Oura::load(http::client()?)?.collection("heartrate", &params)?, tz, start, end);
    if r.json {
        return json_out(tz, start, end, &BTreeMap::from([("heartrate".to_string(), samples)]), &BTreeMap::new());
    }
    let buckets = views::hr_buckets(&samples, tz, single);
    if single {
        println!("Heart rate · {} by hour ({})", start.format("%a %Y-%m-%d"), tz.name());
    } else {
        println!("Heart rate · {start} → {end} ({})", tz.name());
    }
    if buckets.is_empty() {
        println!("no samples");
    } else {
        print!("{}", views::render_hr(&buckets));
    }
    Ok(())
}

fn show_day(day: NaiveDate, json: bool, tz: Tz) -> Result<()> {
    let oura = Oura::load(http::client()?)?;
    let results: Vec<(String, Result<Vec<Value>>)> = std::thread::scope(|s| {
        let handles: Vec<_> = views::DAY_COLLECTIONS
            .iter()
            .map(|c| {
                let oura = &oura;
                (c.to_string(), s.spawn(move || oura.docs_in(c, day, day)))
            })
            .collect();
        handles
            .into_iter()
            .map(|(c, h)| (c, h.join().unwrap_or_else(|_| Err(anyhow!("internal error: fetch thread panicked")))))
            .collect()
    });
    let mut collections = BTreeMap::new();
    let mut errors = BTreeMap::new();
    for (c, result) in results {
        match result {
            Ok(docs) => {
                collections.insert(c, docs);
            }
            Err(e) => {
                errors.insert(c, format!("{e:#}"));
            }
        }
    }
    if collections.is_empty() {
        bail!("{}", errors.values().cloned().collect::<Vec<_>>().join("; "));
    }
    if json {
        return json_out(tz, day, day, &collections, &errors);
    }
    print!("{}", views::render_day(day, tz, &collections, &errors));
    Ok(())
}

fn show_me(json: bool, tz: Tz) -> Result<()> {
    let doc = Oura::load(http::client()?)?.collection("personal_info", &[])?.into_iter().next().unwrap_or(Value::Null);
    if json {
        println!("{}", serde_json::to_string_pretty(&doc)?);
    } else {
        print!("{}", views::render_detail(&["age", "biological_sex", "height", "weight", "email"], &doc, tz));
    }
    Ok(())
}

fn expiry(expires_at: i64) -> String {
    let left = expires_at - Utc::now().timestamp();
    if left <= 0 {
        "expired".into()
    } else {
        format!("valid for {}", fmt_duration(left as f64).unwrap_or_default())
    }
}

fn status(tz: Tz) -> Result<()> {
    println!("config:   {}", store::config_dir()?.display());
    println!("timezone: {}", tz.name());
    println!("log:      {}", log::path().map(|p| p.display().to_string()).unwrap_or_else(|| "off".into()));
    let oura_line = match (oura::load_client()?, oura::load_tokens()?) {
        (None, _) => "not configured (somno oura configure)".to_string(),
        (Some(_), None) => "configured, not logged in (somno oura login)".to_string(),
        (Some(_), Some(t)) => format!(
            "logged in; access token {} (refreshes automatically){}",
            expiry(t.expires_at),
            t.scope.map(|s| format!("; scopes: {s}")).unwrap_or_default()
        ),
    };
    println!("oura:     {oura_line}");
    let eight_line = match eight::load_session()? {
        None => "not logged in (somno eight login EMAIL)".to_string(),
        Some(s) => format!(
            "logged in as {}; token {}; password {}",
            s.email,
            expiry(s.expires_at),
            if eight::has_credentials()? { "remembered" } else { "not remembered" }
        ),
    };
    println!("eight:    {eight_line}");
    Ok(())
}

fn prompt(label: &str) -> Result<String> {
    eprint!("{label}");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn oura_cmd(cmd: OuraCmd, tz: Tz, today: NaiveDate) -> Result<()> {
    match cmd {
        OuraCmd::Configure => {
            eprintln!("Your Oura app's Redirect URI must be exactly: {}", oura::REDIRECT_URI);
            let client_id = prompt("Oura client ID: ")?;
            let client_secret = rpassword::prompt_password("Oura client secret: ")?.trim().to_string();
            if client_id.is_empty() || client_secret.is_empty() {
                bail!("client ID and secret are both required");
            }
            let cfg = ClientConfig { client_id, client_secret };
            eprintln!("checking with Oura...");
            let (authorize, secret) = oura::verify_client(&http::client()?, &cfg);
            let mut failed = false;
            let mut unknown = false;
            log::event("oura.configure", format_args!("authorize={authorize:?} secret={secret:?}"));
            for (label, check) in [("redirect URI", &authorize), ("ID + secret ", &secret)] {
                let (mark, text) = match check {
                    oura::Check::Pass(t) => ("ok  ", t),
                    oura::Check::Fail(t) => { failed = true; ("FAIL", t) }
                    oura::Check::Unknown(t) => { unknown = true; ("??  ", t) }
                };
                println!("  {mark} {label}  {text}");
            }
            if failed {
                bail!("not saved: fix the app settings above and run `somno oura configure` again");
            }
            let path = oura::save_client(&cfg)?;
            if unknown {
                println!("saved to {} (0600), but not fully verified; next: somno oura login", path.display());
            } else {
                println!("verified and saved to {} (0600); next: somno oura login", path.display());
            }
        }
        OuraCmd::Login => {
            let tokens = Oura::load(http::client()?)?.login()?;
            println!("logged in to Oura{}", tokens.scope.as_ref().map(|s| format!(" (scopes: {s})")).unwrap_or_default());
            match oura::missing_scopes(tokens.scope.as_deref()) {
                Some(missing) if !missing.is_empty() => println!(
                    "not granted: {} (log in again and leave them ticked if you want that data)",
                    missing.join(", ")
                ),
                _ => {}
            }
            println!("next: somno oura check");
        }
        OuraCmd::Check => {
            let results = Oura::load(http::client()?)?.access_check(today, tz);
            let mut failures = 0;
            for (name, result) in &results {
                match result {
                    Ok(n) => println!("  ok    {name:<26} {n} document(s) in the last few days"),
                    Err(e) => {
                        failures += 1;
                        println!("  FAIL  {name:<26} {e}");
                    }
                }
            }
            if failures > 0 {
                bail!("{failures} of {} Oura collections could not be read", results.len());
            }
            println!("all {} Oura collections readable", results.len());
        }
        OuraCmd::Logout => {
            let removed = oura::logout()?;
            println!("{}", if removed { "Oura tokens deleted" } else { "no Oura tokens were saved" });
        }
    }
    Ok(())
}

fn eight_cmd(cmd: EightCmd) -> Result<()> {
    match cmd {
        EightCmd::Login { email, remember } => {
            // SOMNO_EIGHT_PASSWORD lets login run without a TTY.
            let password = match std::env::var(eight::PASSWORD_ENV) {
                Ok(p) if !p.is_empty() => p,
                _ => rpassword::prompt_password("Eight Sleep password: ")?,
            };
            let session = Eight::login(&http::client()?, &email, &password)?;
            println!("logged in to Eight Sleep as {} (user {}); token {}", session.email, session.user_id, expiry(session.expires_at));
            if remember {
                let path = eight::save_credentials(&Credentials { email, password })?;
                println!("password saved to {} (0600) for automatic renewal", path.display());
            }
        }
        EightCmd::Logout => {
            let removed = eight::logout()?;
            println!("{}", if removed { "Eight Sleep session deleted" } else { "no Eight Sleep session was saved" });
        }
    }
    Ok(())
}

fn raw(cmd: RawCmd, tz: Tz, today: NaiveDate) -> Result<()> {
    let value = match cmd {
        RawCmd::Oura { collection, start, end } => {
            let params = oura::window_params(&collection, start.map(|d| d.resolve(today)), end.map(|d| d.resolve(today)), tz)?;
            let docs = Oura::load(http::client()?)?.collection(&collection, &params)?;
            if collection == "personal_info" {
                docs.into_iter().next().unwrap_or(Value::Null)
            } else {
                json!({"collection": collection, "query": params.into_iter().collect::<std::collections::BTreeMap<_, _>>(), "data": docs})
            }
        }
        RawCmd::Eight { endpoint, start, end } => {
            let mut client = Eight::load(http::client()?)?;
            match endpoint {
                EightEndpoint::Me => client.me()?,
                EightEndpoint::Trends => {
                    let (Some(start), Some(end)) = (start, end) else {
                        bail!("trends needs --start and --end");
                    };
                    client.trends(start.resolve(today), end.resolve(today), tz.name())?
                }
            }
        }
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
