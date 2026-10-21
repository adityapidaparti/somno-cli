# somno

Fast local CLI for reading your sleep data from [Oura](https://ouraring.com)
and [Eight Sleep](https://www.eightsleep.com). It's the sleep counterpart to
`crono`. It's **read-only**: it never changes anything on your ring, bed or
accounts. There's no background process; it authenticates on demand and caches
tokens on disk.

## Install

```sh
cargo install --path .
```

## Setup

### Oura (official API V2, OAuth)

1. Create a personal application at <https://developer.ouraring.com/>.
   - Redirect URI, exactly this with nothing after it (no trailing period
     or slash):

     ```
     http://localhost:8765/callback
     ```

     The portal accepts plain HTTP only for the literal `localhost`
     hostname. If it's off by a character, `configure` tells you what's
     registered instead.
   - Scopes: tick all eleven (Email, Personal, Daily, Heartrate, Tag,
     Workout, Session, SpO2, Ring Configuration, Stress, Heart Health).
   - The form also asks for website, privacy and terms URLs.
   - Gen3 and later rings need an active Oura membership for API access.
2. Save the app's credentials, then authorize in the browser:

   ```sh
   somno oura configure   # prompts (the secret is hidden), checks with Oura, saves only if they pass
   somno oura login       # prints the consent URL (and opens it on a local Mac)
   somno oura check       # reads every data type once and shows what your scopes reach
   ```

   `configure` checks two things with Oura before saving, with no login:
   - **Redirect URI:** the app's authorization page must accept the redirect
     URI. A rejection there means the browser login would fail too.
   - **Client ID and secret:** a token request with a deliberately invalid
     code must come back `invalid_grant` (credentials fine, code refused),
     not `invalid_client`.

   A definite failure saves nothing. A network error or unexpected answer
   saves with a warning.

   `login` works on any machine. If the browser runs on the same machine, the
   local callback finishes the login by itself. If it doesn't (SSH, a server,
   a bot's host), approve in a browser anywhere. The browser then lands on a
   `localhost:8765` page that fails to load; paste that page's full address
   into the terminal. Either way the `state` check applies and it waits up to
   10 minutes. No browser is opened over SSH or with `SOMNO_NO_BROWSER=1`.

   The authorization request names all eleven scopes explicitly. Oura's docs
   say a blank scope means "all available", but they list only eight and omit
   `stress`, `heart_health` and `ring_configuration`. If you untick any on
   the consent screen, `login` lists what wasn't granted. `night` still works
   without the daily scores, and prints a note saying what it couldn't read.

### Eight Sleep (unofficial app API, password login)

Eight Sleep has no public API. somno uses the private API the mobile app uses,
the same way open-source clients do (pyEight in
[lukas-clarke/eight_sleep](https://github.com/lukas-clarke/eight_sleep),
[steipete/eightctl](https://github.com/steipete/eightctl)). It can change
without notice. Logins are rate-limited, so the session token is reused until
it expires.

```sh
somno eight login you@example.com             # prompts for the password
somno eight login you@example.com --remember  # also store it (0600) for automatic renewal
```

Without `--remember`, run `login` again when the session expires.
`SOMNO_EIGHT_PASSWORD` supplies the password without a prompt.

## Usage

```sh
somno night               # the night ending this morning, both sources side by side
somno night yesterday
somno night 2026-10-12
somno week                # seven nights ending today, one row per source
somno range 2026-10-01 2026-10-14
somno night --json        # normalized JSON (missing values are null)
somno week --only oura    # one source
somno status              # what's configured and when tokens expire (no network)
somno log                 # the last 40 log lines (-n for more)

# every Oura data type
somno day                 # everything for today on one screen (also: somno day yesterday)
somno readiness           # no dates: the last 7 days as a table
somno readiness yesterday # one day: every field, contributors included
somno activity 2026-10-01 2026-10-14   # two days: that range
somno sleep               # every sleep period, naps included
somno stages              # last night's stages: hypnogram, time per stage, onset, wake-ups, movement
somno stages yesterday --segments   # plus every stage change at 30-second precision
somno sleep-score | stress | resilience | spo2 | cardio-age | vo2max
somno workouts | sessions | tags | bedtime | rest-mode
somno hr                  # heart rate min/avg/max per day; `somno hr yesterday` is per hour
somno me                  # age, sex, height, weight, email
somno activity --json     # Oura's original documents for those days, by collection

# original provider JSON, unmodified
somno raw oura sleep --start 2026-10-01 --end 2026-10-08
somno raw oura heartrate --start yesterday --end today
somno raw eight trends --start 2026-10-01 --end 2026-10-08
somno raw eight me
```

`--tz ZONE` (or `SOMNO_TZ`) sets the zone for "today" and displayed times;
the default is the system zone. Eight Sleep's trends endpoint also takes it.
Oura bedtimes, workouts and sessions show local clock time where they
happened, with the UTC offset when it differs from `--tz`. Heart-rate samples
carry UTC times, so `hr` groups them by day and hour in `--tz`. The repeated
hour when clocks fall back appears twice, labelled with its offset.

Every Oura collection has a command except the ring's battery level and
hardware configuration (`somno raw oura ring_battery_level` /
`ring_configuration`). The single-day view prints every field in Oura's
documents, formatted with units, so fields Oura adds later still appear.
Sample series (heart rate, HRV, MET) are summarized as count, average, min and
max. Encoded series (`movement_30_sec`, `class_5_min`) are noted; the full
values are in `--json`. Days with no data say so; a value Oura didn't send is
shown as `—`, never 0.

`stages` decodes each sleep period's stage codes (`sleep_phase_30_sec`, else
`sleep_phase_5_min`, else `app_sleep_phase_5_min`: 1 deep, 2 light, 3 REM,
4 awake) and `movement_30_sec` (still, restless, tossing and turning,
active). On real data (2026-10-21) the 30-second codes reproduced Oura's
deep, light and REM durations exactly, and awake to within one epoch.
"Fell asleep" is Oura's onset (`bedtime_start + latency`). Brief dozing before
it is listed separately. Awake time and awakenings are counted from that same
onset to the last epoch scored as sleep, so falling asleep isn't counted as
waking up. A latency longer than the period is treated as unknown.
Stretches and "woke for good" are counted from the codes. Times stay in the offset the period started in, with a
note when the timezone changed during the night.

`somno raw oura` reads all 19 data collections in Oura's API (OpenAPI 1.42,
checked 2026-10-20). Oura's spec doesn't say which scope unlocks which
collection. The scope column below is inferred from the names and scope
descriptions; `somno oura check` shows what your login actually reaches.

| Scope (inferred) | Collections |
|---|---|
| `email`, `personal` | `personal_info` |
| `daily` | `daily_sleep`, `daily_readiness`, `daily_activity`, `sleep`, `sleep_time`, `rest_mode_period` |
| `heartrate` | `heartrate` (time series) |
| `workout` | `workout` |
| `tag` | `tag`, `enhanced_tag` |
| `session` | `session` |
| `spo2` | `daily_spo2` |
| `stress` | `daily_stress`, `daily_resilience` |
| `heart_health` | `daily_cardiovascular_age`, `vO2_max` |
| `ring_configuration` | `ring_configuration`, `ring_battery_level` |

`--start`/`--end` are sent as Oura's own `start_date`/`end_date`, with Oura's
semantics. For `heartrate` and `ring_battery_level` they become
`start_datetime`/`end_datetime` at local midnight. `personal_info` and
`ring_configuration` are not date-filtered.

Not covered:
- Single-document routes (`/{collection}/{id}`): the list routes return the
  same documents.
- Oura's `sandbox` sample-data routes.
- Webhook subscriptions: they need a public HTTPS server to receive pushes.

## Bots and other machines

Run `somno` on the machine where the bot or job runs, and log in there once.
Use the paste step above when it is remote. After that it refreshes on its
own with no browser. **Keep one copy of the Oura login.** Oura refresh tokens
are single-use and rotate on every refresh. If two machines refresh copies of
the same tokens, one of them is locked out and must log in again. To move a
login, move `oura-client.json` and `oura-tokens.json`; don't copy them.
Eight Sleep only needs `eight login --remember` on that machine. Pass `--tz`
(or set `SOMNO_TZ`) if it's in a different zone from you.

Agents use it as a plain CLI; there is no MCP server. The skill at
[`skills/somno/SKILL.md`](skills/somno/SKILL.md) covers the commands, the
JSON shape and the ground rules (never run login commands, missing is not
zero, keep sources separate). Claude Code and Grok CLI both load skills from
`~/.claude/skills`, so link it there:

```sh
ln -s "$PWD/skills/somno" ~/.claude/skills/somno
```

## What a night means

- Each source's own `day` label is used as-is; the two are not forced to agree.
  Bedtimes are printed so a mismatch is visible. `night`, `week` and `range`
  query one extra day on each side and filter by that label, so they don't
  depend on either API's end-date inclusivity.
- **Oura:** the main period is the longest `long_sleep` that day. If there is
  none, it's the longest other confirmed period, and the period type is
  shown. Ties go to the earliest bedtime. Other confirmed periods (naps) are
  counted, not added in. Per Oura's spec, `deleted` periods were deleted by
  you and `rest` periods are false detections you rejected. Neither ever
  counts as sleep, but both still appear in `somno sleep`. Sleep score
  comes from `daily_sleep`; readiness and temperature deviation come from
  `daily_readiness`.
- **Eight Sleep:** one trends entry per night, read from the night's main
  session (`mainSessionId`). Checked against real responses on 2026-10-21:
  - Durations come from the session's `stageSummary`, which adds up: asleep
    + awake + out of bed = time in bed. "awake" is time awake in bed,
    including falling asleep; "out of bed" is separate.
  - HR, HRV and breathing are `sleepQualityScore.*.current`, the night
    itself. `.average` is Eight Sleep's rolling baseline: one night's
    `average` equals the previous night's `current`. The open-source clients
    read `average`, and so did somno until that check.
  - Lowest HR is the lowest heart-rate sample of the session; bed and room
    temperature are the mean of its samples.
  - Days without a session fall back to the day-level fields. Awake and out
    of bed are then unknown.
  - A "still processing" note means Eight Sleep hasn't finished computing
    that night (`processing` or `incomplete`).
- Eight Sleep detects you on the mattress, so its time in bed usually starts
  earlier than Oura's, and the two can disagree on how much was sleep. Each
  number is shown with its source; neither is treated as the truth.
- Missing means unknown, never zero: `—` in tables, `null` in JSON.

## Log

Every run appends to `somno.log` in the config directory (0600; rotated to
`somno.log.1` past 1 MiB). Read it with `somno log`. It records:
- the command and its arguments
- each HTTP request: method, host, path and query, status, duration, size,
  and the provider's short error code and message
- login steps: listener, callbacks, pastes, token endpoint fallback, granted
  scopes
- token refreshes, Eight Sleep re-logins and rate-limit retries
- the final error

Never logged: secrets, passwords, tokens, authorization codes, OAuth state,
client IDs (`…` in URLs) or response bodies. Set `SOMNO_LOG=off` to disable
it, or `SOMNO_LOG=<path>` to write elsewhere.

## Local state and security

- Config, tokens and the optional remembered password live in
  `~/Library/Application Support/somno/` (directory 0700, files 0600, atomic
  writes). Override the location with `SOMNO_CONFIG_DIR`.
- Requests only go to `api.ouraring.com`, `moi.ouraring.com`,
  `cloud.ouraring.com` (the `configure` check only, no credentials),
  `auth-api.8slp.net` and `client-api.8slp.net` over HTTPS, and never follow
  redirects. Errors show the HTTP status and a short server message, never
  response bodies.
- Oura tokens come from `moi.ouraring.com/oauth/v2/ext/oauth-token`, falling
  back to the documented `api.ouraring.com/oauth/token`. The documented
  endpoint rejects new-portal apps (reported since 2026-10-09 by the Home
  Assistant Oura integration, HealthLog and oura-reader). The live one goes
  first because a login code can be used only once.
- Oura refresh tokens are single-use. Refreshes run under a file lock and the
  rotated pair is saved before use, so concurrent invocations don't break
  the login. Eight Sleep re-login on a rejected token is locked the same way.
- The OAuth callback listener binds `127.0.0.1:8765` only. It rejects
  callbacks with a missing or mismatched state, or with repeated parameters.
  `SOMNO_NO_BROWSER=1` skips opening the browser and just prints the URL.
- The Eight Sleep client ID in the source is the mobile app's own, published
  in every open-source client. Your password is the secret.

## Testing

`cargo test` runs example tests (fixtures shaped like real responses) and
property-based tests (`tests/properties.rs`, proptest). The properties follow
Hughes, "How to Specify It!" (2019). His comparison found model-based and
metamorphic properties catch bugs fastest, while validity-only properties
missed most planted bugs.

- **Model-based.** Generators build the truth first and derive the provider
  JSON from it, for example a night as runs of sleep stages, or a set of
  periods across days. The code's answer is then compared with the model:
  stage totals, stretches, onset and wake times, segments, chart columns,
  the nightly pick, pagination and heart-rate buckets.
- **Metamorphic.** Change the input in a known way and compare the outputs.
  Shuffled API order gives the same nights. A range equals its two halves.
  The result doesn't depend on whether Oura's date bounds are inclusive.
  The same instants written in another UTC offset give the same results.
- **Security postconditions.** Sensitive query values never reach the log
  in any encoding. An OAuth redirect is accepted only with its exact state,
  and any code survives the round trip.
- **Generators are tested too.** Generated data is internally consistent,
  and a measured distribution check makes sure the interesting cases happen:
  tied sleep periods, naps, deleted periods, mid-night timezone changes, runs
  at the 5-minute threshold, and empty nights. Values come from small domains
  so ties and collisions are common.

Failing inputs are saved in `proptest-regressions/` (committed) and replayed
first. For a deeper run: `PROPTEST_CASES=5000 cargo test --test properties`.
To check that the tests catch bugs, run mutation testing with
`cargo mutants` (cargo-mutants), which plants small bugs and reports any that
no test catches.

Bugs found this way, all fixed and kept as tests:
- When two sleep periods tied for longest, the "main" night depended on the
  order Oura returned them. Ties now go to the earliest bedtime, compared to
  the millisecond.
- When Eight Sleep returned duplicate days, the result depended on response
  order.
- Rest-mode periods and tags with a duration that started more than a day
  before the queried range were missed. Span collections now look back a
  year. An enhanced tag without an end day is a point in time, not an
  ongoing span.
- A rejected `rest` period could be picked as the night.
- The `day` dashboard said "none" or "off" for collections it couldn't read.
- Wake-ups were counted from a different onset than "fell asleep" showed.
- The two 01:00 hours of a DST fall-back were merged in `hr`.
- `-0` didn't round-trip; `--1` parsed as one day ahead, and a huge offset
  panicked.
- An absurd latency, or an absurd token expiry, overflowed and panicked.

Mutation testing (cargo-mutants 27.1, 252 mutants in the core functions)
measured the suite. The first suite caught 204 of the 230 mutants that
compiled. The misses pointed at the chart axis, Eight Sleep values, OAuth
state comparison, empty codes and tokens, and day parsing, and each now has a
property.

## Verification status

`cargo test` runs 86 tests (42 of them property tests): example tests over synthetic fixtures (summaries,
OAuth callback/state, pasted redirect URLs, configure verdicts, scopes,
token rotation, pagination, DST windows, host allowlist, log redaction,
rendering, and the Oura views: day selection and spans, ordering, units,
detail and table layout, bedtime windows, tags, workouts and sessions,
heart-rate bucketing, the day dashboard, stage decoding, totals, onset,
awakenings, movement, the hypnogram and stage JSON), property tests (parsers and summaries
are total over arbitrary input and never invent values) and private-file
storage and logging. `configure` has run live against Oura:
- With the real secret, the ID + secret check passed (`invalid_grant`).
- With a wrong secret, it failed with `invalid_client`.

In both runs the redirect check failed and nothing was saved. The app's
Redirect URI turned out to be registered as `http://localhost:8765/callback.`,
with a stray trailing period. Oura accepted the authorization request with
that exact value (302 to sign-in) and rejected it without (400). `configure`
now tries such near-misses on failure and names the registered value. The login was also exercised by hand with a fake client:

- On the loopback callback, a wrong state is rejected and a decline ends the
  login. This works even with stdin closed.
- With the port unavailable, a stale pasted URL is rejected and a valid paste
  completes the login.
- With no port and no terminal, it fails immediately.
- No token was written in any case.

**Verified live (2026-10-21):**

- Oura login (new developer portal, `moi.ouraring.com` token endpoint), all
  19 collections readable. A week of nights matched Oura's raw records field
  for field. Stage codes reproduced Oura's stage durations.
- Eight Sleep login. Two nights' mapping was checked against the response's
  own internal consistency, as described above. It has not been compared
  with the Eight Sleep app's screens.
