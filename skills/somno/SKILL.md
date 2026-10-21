---
name: somno
description: The user's Oura and Eight Sleep data through the read-only `somno` CLI. Covers sleep (last night, a week, a range; both sources side by side) and every other Oura data type with its own command, such as readiness, activity and steps, stress and resilience, workouts, heart rate, SpO2, VO2 max, cardiovascular age, tags, sessions, and a one-day dashboard. Use for "how did I sleep", HRV, resting heart rate, sleep stages, readiness, stress, workouts, or comparing Oura with Eight Sleep.
---

# Oura and Eight Sleep data (somno)

Read-only CLI, on PATH. Run one `somno` command per call. Use `--json` when you'll compute or reformat; the plain table is for showing as-is.

```
somno night --json                  # the night ending today
somno night yesterday --json        # also -N or YYYY-MM-DD
somno week --json                   # 7 nights ending today
somno range 2026-10-01 2026-10-14 --json
somno night --only oura             # or --only eight
somno status                        # setup and token expiry, no network
somno raw oura sleep --start 2026-10-18 --end 2026-10-20    # original JSON, any Oura collection below
somno raw eight trends --start 2026-10-18 --end 2026-10-20  # original Eight Sleep JSON
somno oura check                    # which Oura collections this login can read
somno log                           # recent requests, statuses, error codes and login steps (no secrets)
```

Every Oura data type has its own command. No dates gives the last 7 days as a table, one day gives every field for that day, and `START END` gives a range. `--json` returns `{"timezone", "start", "end", "collections": {<collection>: [Oura's original documents]}, "errors"}`.

```
somno day [DAY] [--json]       # everything for one day: readiness, sleep, activity, stress, resilience, SpO2, cardio age, VO2 max, bedtime, workouts, sessions, tags
somno readiness [DAY|START END]    # score, temperature deviation, contributors
somno sleep [...]              # every sleep period incl. naps: stages, HR/HRV, efficiency, embedded readiness
somno stages [DAY] [--json]    # sleep stages through each period: totals, onset, first deep/REM, awakenings, movement, every 30-second segment
somno sleep-score [...]        # daily sleep score + contributors
somno activity [...]           # steps, active/total kcal, activity minutes, score + contributors
somno stress [...]             # high-stress / high-recovery time, day summary
somno resilience [...]         # level + contributors
somno spo2 [...]               # overnight SpO2 average, breathing disturbance index
somno cardio-age [...]         # vascular age, pulse wave velocity
somno vo2max [...]
somno workouts [...]           # activity, time, duration, kcal, distance, intensity, source
somno sessions [...]           # meditation/breathing/etc.: type, mood, HR and HRV averages
somno tags [...]               # tags and notes from the Oura app
somno bedtime [...]            # Oura's recommended bedtime window
somno rest-mode [...]
somno hr [DAY|START END]       # heart rate min/avg/max per day, or per hour for one day (max 31 days)
somno me                       # age, sex, height, weight, email
```

Oura's ring battery and hardware config are only under `somno raw oura ring_battery_level|ring_configuration`. `somno raw oura <collection>` returns any collection's documents unfiltered.

JSON shape: `days[]` has `day` plus `oura` and `eight_sleep`. Each has `status`, which is one of:
- `record`, with a `night` object
- `no_record`
- `unavailable`, with a `reason`

`night` fields:
- Durations in seconds: `asleep_s`, `deep_s`, `rem_s`, `light_s`, `awake_s`, `time_in_bed_s`, `latency_s`.
- Metrics: `hr_avg_bpm`, `hr_lowest_bpm`, `hrv_avg_ms`, `breath_avg_per_min`, `sleep_score`, `readiness_score`, `temp_deviation_c`, `bed_temp_c`, `room_temp_c`, `out_of_bed_s` (Eight Sleep).
- Times: `bedtime_start` and `bedtime_end`, in RFC 3339.
- `null` means the source didn't report it.

Rules:
- Never run `somno oura configure|login|logout` or `somno eight login|logout`; they need the user's browser or password. `somno oura check` is fine to run. When a source is `unavailable`, pass on its `reason`, which names the command the user should run.
- When a command fails, read `somno log` before guessing. It shows each request's status and Oura's or Eight Sleep's error code.
- A collection that fails with HTTP 401/403 usually means its scope wasn't granted. Say so, and suggest the user log in again with every scope ticked.
- Missing is unknown, not zero. Don't fill gaps, and don't merge the two sources into one number. Report each value with its source.
- Each source labels a night by its own `day`. Check `bedtime_start`/`bedtime_end` before comparing a night across sources.
- Oura's night is its longest `long_sleep`. `other_periods` counts confirmed naps left out (always `null` for Eight Sleep). Periods typed `deleted` (you deleted them) or `rest` (false detections you rejected) never count as sleep, but `somno sleep` still lists them.
- `somno day` marks a section "unavailable" when its data couldn't be fetched. That is not the same as none.
- Eight Sleep uses an unofficial API that can change. Its "awake" is awake in bed; "out of bed" (`out_of_bed_s`) is separate. Eight Sleep usually counts more time in bed and more sleep than Oura (it detects the mattress, not the ring). Report both with their source, and don't call either one correct. Nights with `processing: true` aren't final.
- "today" and displayed times use the system timezone. Pass `--tz America/New_York` if this machine is in another zone.
- Describe sleep without moralizing.
