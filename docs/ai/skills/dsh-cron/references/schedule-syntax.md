# Schedule syntax

`cron add` and `cron edit --schedule` accept three shapes in one argument:

| Form | Example | Meaning |
|---|---|---|
| Interval | `30s`, `5m`, `1h` | Every N seconds/minutes/hours, measured from the previous run. Range: 5s-24h. |
| Cron expression | `0 9 * * mon-fri` | Five fields: minute hour day-of-month month day-of-week, on the local wall clock. |
| Macro | `@hourly`, `@daily`, `@midnight`, `@weekly`, `@monthly`, `@yearly`/`@annually` | Shorthand for a fixed cron expression. |

## Quoting

**Always quote a cron expression.** `cron add */5 * * * * git fetch` is five separate
shell-expanded words by the time dsh sees them - `*/5` survives, but the bare `*` fields
get glob-expanded against the current directory's filenames. `cron add` detects the
common shapes of this mistake and returns an error naming the fix, but the safe habit is
to always write `cron add '*/5 * * * *' git fetch`.

An interval (`5m`) or a macro (`@daily`) never needs quoting - neither contains a shell
metacharacter - but quoting them anyway costs nothing and keeps the habit uniform.

## Field syntax

Each of the five cron fields accepts:

- `*` - every value
- `n` - exactly one value
- `a-b` - an inclusive range
- `*/n` or `a-b/n` - a step
- comma-separated combinations of the above: `1,15`, `9-17/2`, `mon,wed,fri`

Month accepts `jan`-`dec`; day-of-week accepts `sun`-`sat` (case-insensitive), and `0`
and `7` both mean Sunday. **Only Vixie-cron syntax is supported** - no seconds field, and
none of Quartz's `L`, `W`, `#`, or `?`. Those characters produce an explicit parse error
rather than being silently reinterpreted.

## The day-of-month / day-of-week trap

When **both** the day-of-month and day-of-week fields are restricted (neither is `*`),
cron treats them as **OR**, not AND: `0 0 1 * mon` means "the 1st of the month, and every
Monday" - not "Mondays that fall on the 1st". This is the single most common source of a
schedule that fires more often than intended. If you only meant one of the two
conditions, leave the other field as `*`.

## Timezone and DST

Cron expressions run on the shell's local wall clock (`chrono::Local`), not UTC. Around a
daylight-saving transition:

- **Spring-forward gap** (a local time that never happens, e.g. 02:30 on the day clocks
  jump to 03:00): the job fires at the first instant that does exist - here, 03:00 - not
  a day late.
- **Autumn overlap** (a local time that happens twice): the job fires once, at the
  earlier of the two instants.

A schedule that can never match (`0 0 30 2 *` - February never has a 30th) is detected:
`cron doctor` reports it, and internally the search for a next run gives up after
looking ten years ahead rather than looping forever.

## Catch-up after downtime

Each job has a `--catchup` window (default one hour). If the machine was asleep, or no
dsh session and no external tick were running, past due slots are **not** replayed one
by one - a five-minute job that missed eight hours does not fire ninety-six times in a
row. Everything older than the catch-up window collapses into a single run, and the next
slot is computed forward from there.

## `--timeout`

`--timeout` (accepted on `add` and `edit`) bounds one run's wall-clock duration - seconds,
or an interval string like `2m`. For a shell job it is the hard kill deadline. For an
interval schedule, `--timeout` is capped to the interval itself (a run must not outlive
its own next scheduled slot); a cron-expression schedule has no fixed interval to compare
against, so its timeout is not auto-capped.
