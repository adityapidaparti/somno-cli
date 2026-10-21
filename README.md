# somno

Read your [Oura](https://ouraring.com) and [Eight Sleep](https://www.eightsleep.com)
sleep data from the terminal. somno only reads. It never changes anything on
your ring, your bed or your accounts.

```console
$ somno night
Night ending Tue 2026-10-13 (America/New_York)
                  Oura              Eight Sleep
in bed            23:05 → 07:10     22:40 → 07:18
asleep            7h 02m            7h 41m
  deep            1h 15m            1h 31m
  REM             1h 38m            1h 52m
  light           4h 09m            4h 18m
HRV avg           62 ms             60 ms
sleep score       84                88
...
```

## Install

You need [Rust](https://rustup.rs).

```sh
cargo install --locked --git https://github.com/adityapidaparti/somno-cli
```

### With an AI agent

Give your agent this repository's link and ask it to install somno. It should:

1. Run `cargo install --locked --git https://github.com/adityapidaparti/somno-cli`.
2. Run `somno skill install`, which adds the somno skill for Claude Code and
   Grok. For Codex, run `somno skill install --dir ~/.agents/skills`.
3. Ask you to do the [setup](#set-up) yourself, since it needs your browser or
   password.

## Set up

**Oura.** Create an app at <https://developer.ouraring.com> with the redirect
URI `http://localhost:8765/callback` and every scope ticked. Then:

```sh
somno oura configure    # enter the app's client ID and secret
somno oura login        # approve access in the browser
```

**Eight Sleep.**

```sh
somno eight login you@example.com --remember
```

## Use

```sh
somno night                          # last night, both sources side by side
somno week                           # the last seven nights
somno range 2026-10-01 2026-10-14
somno stages                         # last night's sleep stages
somno readiness                      # likewise sleep, activity, stress, hr, workouts and more
somno day                            # today's Oura summary
somno night --json                   # JSON, for scripts and agents
somno raw oura sleep --start 2026-10-01 --end 2026-10-08   # Oura's original JSON
```

`somno --help` lists every command.

## Good to know

- Eight Sleep has no public API. somno uses the one Eight Sleep's app uses,
  which can change without notice.
- somno shows each source's numbers as that source reports them. A value a
  source didn't send shows as `—`, never 0.
- `--remember` saves your Eight Sleep password, unencrypted, in a file only
  your user can read, so somno can log in again by itself. Without it, you log
  in again when the session expires. `somno eight logout` deletes a saved
  password.
- Keep one Oura login. Oura's refresh tokens work once, so two machines sharing
  a login lock each other out.

## Tests

Tested with [Hegel](https://hegel.dev) property-based tests.
