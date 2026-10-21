---
name: somno
description: Read the user's Oura and Eight Sleep data with the read-only `somno` CLI. Use for "how did I sleep", sleep stages, HRV, resting heart rate, heart rate, readiness, activity, stress, resilience, workouts, SpO2, VO2 max, cardiovascular age, tags, sessions, or comparing Oura with Eight Sleep.
---

# Oura and Eight Sleep data

`somno` is a read-only CLI on PATH. Run one `somno` command per call. Use `--json` when you will compute or reformat, and show the plain tables as they are.

## First check

Run `somno status`. It reads only local files and shows which sources are set up and when their tokens expire. For a source the user wants but hasn't set up, give them these steps to run in their own terminal:

- Oura: create an app at https://developer.ouraring.com. Its redirect URI must be exactly `http://localhost:8765/callback`, and every scope must be ticked. Then run `somno oura configure`, which asks for the app's client ID and secret and checks them with Oura, and `somno oura login`, which opens Oura's consent page or prints its link.
- Eight Sleep: `somno eight login <their email> --remember`. `--remember` saves their password unencrypted so somno can log in again by itself. They can leave it off.

Afterwards, check only the sources they set up. `somno oura check` shows which Oura data types the login reaches, and `somno night --only eight` reads last night from Eight Sleep.

## Nights from both sources

```
somno night --json                       # the night ending today
somno night yesterday --json             # a day is today, yesterday, -N or YYYY-MM-DD
somno week --json                        # seven nights ending today
somno range 2026-10-01 2026-10-14 --json # up to 400 nights
somno night --only oura                  # or --only eight
```

The JSON is `{"timezone", "days", "notes"}`. Each entry in `days` has `day` and one key per source, `oura` and `eight_sleep`. With `--only`, the other key is absent. Each source has a `status`:
- `record`, with a `night` object
- `no_record`
- `unavailable`, with a `reason`

`night` fields:
- Durations in seconds: `asleep_s`, `deep_s`, `rem_s`, `light_s`, `awake_s`, `time_in_bed_s`, `latency_s`, and `out_of_bed_s`, which only Eight Sleep reports.
- Metrics: `hr_avg_bpm`, `hr_lowest_bpm`, `hrv_avg_ms`, `breath_avg_per_min`, `sleep_score`, `readiness_score`, `temp_deviation_c`, `bed_temp_c`, `room_temp_c`.
- Times: `bedtime_start` and `bedtime_end`, in RFC 3339.
- `other_periods`: how many other confirmed Oura periods that day has, such as naps. It is `null` for Eight Sleep.
- `null` means unknown. Before treating the results as complete, check `notes` for requests that failed.

## Every Oura data type

```
somno day [DAY] [--json]          # one day on one screen; defaults to today
somno readiness [DAY | START END] # score, temperature deviation, contributors
somno sleep                       # every sleep period, naps included
somno stages [DAY] [--json]       # stages through each period: totals, onset, first deep and REM, wake-ups, movement
somno stages [DAY] --segments     # also every stage change
somno sleep-score
somno activity                    # steps, calories, activity minutes, score
somno stress
somno resilience
somno spo2
somno cardio-age
somno vo2max
somno workouts
somno sessions                    # meditation, breathing and other sessions
somno tags
somno bedtime                     # Oura's recommended bedtime window
somno rest-mode
somno hr [DAY | START END]        # heart rate per day, or per hour for one day; up to 31 days
somno me [--json]                 # age, sex, height, weight, email
```

Commands that take dates show the last seven days as a table when given none, one day in detail, or an inclusive range of up to 400 days, or 31 for `hr`. `day` and `stages` default to today, and `me` takes no date. With `--json` they print `{"timezone", "start", "end", "collections", "errors"}`, where `collections` maps each collection to Oura's original documents and `errors` lists collections that failed. `somno me --json` prints Oura's profile as is, and `somno stages --json` prints `{"day", "periods"}`.

The detail view leaves out ids and summarizes series. Use `--json` when you need every value.

## Original JSON

```
somno raw oura sleep --start 2026-10-18 --end 2026-10-20   # any of Oura's 19 collections
somno raw oura ring_battery_level --start yesterday --end today
somno raw eight trends --start 2026-10-18 --end 2026-10-20
somno raw eight me
```

Raw Oura queries pass the dates to Oura as given. For `heartrate` and `ring_battery_level`, `--end` is midnight at the start of that date, so use the next date to include a whole last day.

## When something fails

- Run `somno log` before guessing. It shows each request's status and the provider's error code and message.
- When a source is `unavailable`, pass on its `reason`. It names the command the user should run.
- HTTP 401 from Oura means it didn't accept the login, so the user should run `somno oura login` again. HTTP 403 usually means a missing scope or a lapsed membership. The user should log in again with every scope ticked, and then you can run `somno oura check`.
- `somno day` marks a section "unavailable" when its data couldn't be fetched. That is different from no data.

## Rules

- Never run `somno oura configure`, `somno oura login`, `somno oura logout`, `somno eight login` or `somno eight logout`. They need the user's browser, secret or password. `somno oura check` and `somno status` are safe.
- Missing is unknown, not zero. Don't fill gaps, and don't merge the two sources into one number. Report each value with its source.
- Each source labels a night by its own `day`. Compare `bedtime_start` and `bedtime_end` before comparing a night across sources.
- Oura's night is the day's longest `long_sleep`, or its longest other confirmed period when there is none. Periods typed `deleted`, which the user deleted, and `rest`, which are false detections the user rejected, never count as sleep. `somno sleep` still lists them.
- Eight Sleep uses an unofficial API that can change. Its awake time is awake in bed, and out of bed is separate. It senses the mattress while Oura reads the finger, so the two often disagree. Report both with their source, and don't call either one correct. A night with `processing: true` isn't final.
- "today" uses `--tz`, then `SOMNO_TZ`, then the system timezone. Pass `--tz ZONE` when the machine is in a different zone from the user. Oura's times keep the offset Oura recorded.
- Describe sleep without moralizing.

## Updating this skill

To update this file after upgrading somno, run the same `somno skill install` command, with the same `--dir` if one was used. It leaves a linked skill folder alone and replaces a file that differs.
