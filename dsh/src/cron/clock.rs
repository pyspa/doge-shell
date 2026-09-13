//! The only place cron touches a calendar.
//!
//! `dsh-types` deals in [`CivilTime`] - a wall-clock stamp with no offset - so
//! that the leaf crate stays dependency-free. Everything here is the bridge:
//! civil time in, Unix seconds out, `chrono` confined to this file.
//!
//! Two of the three conversions are uninteresting. The third is daylight
//! saving, where a local stamp can name **no** instant (the spring-forward
//! gap) or **two** (the autumn overlap), and picking wrong means a job that
//! silently skips a day or fires twice. [`resolve_local`] is split out as a
//! pure function precisely so those two cases can be tested without waiting
//! for October: `chrono::Local` caches the host time zone on first use, so a
//! test that sets `TZ` is only reliable if it runs first.

#[cfg(test)]
mod tests;

use chrono::{
    DateTime, Datelike, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Timelike,
};
use dsh_types::cron::{CivilTime, CronExpr};
use dsh_types::schedule::Schedule;

/// How far past a gap [`resolve_local`] will look for a real instant.
///
/// Every DST jump in use is an hour; Lord Howe Island's is thirty minutes.
/// A day of slack costs nothing and stops a malformed zone from looping.
const GAP_PROBE_MINUTES: i64 = 24 * 60;

/// Turns a local wall-clock stamp into an instant, resolving DST.
///
/// `lookup` is [`Local::from_local_datetime`] in production and a hand-built
/// table in tests.
///
/// - **Overlap** (the hour after a fall-back happens twice): take the earlier
///   instant. Taking both would run the job twice for one slot; taking the
///   later one would delay it by an hour for no reason.
/// - **Gap** (the hour after a spring-forward does not exist): take the first
///   instant that does exist, which is where the clock jumped to. This matches
///   Vixie cron and systemd timers - a 02:30 job still runs on the day 02:30
///   never happens.
pub fn resolve_local<F>(naive: NaiveDateTime, lookup: F) -> Option<i64>
where
    F: Fn(NaiveDateTime) -> LocalResult<DateTime<Local>>,
{
    let mut candidate = naive;
    for _ in 0..=GAP_PROBE_MINUTES {
        match lookup(candidate) {
            LocalResult::Single(at) => return Some(at.timestamp()),
            LocalResult::Ambiguous(earlier, _) => return Some(earlier.timestamp()),
            LocalResult::None => {
                candidate = candidate.checked_add_signed(chrono::Duration::minutes(1))?;
            }
        }
    }
    None
}

fn local_lookup(naive: NaiveDateTime) -> LocalResult<DateTime<Local>> {
    Local.from_local_datetime(&naive)
}

fn naive_from_civil(civil: CivilTime) -> Option<NaiveDateTime> {
    NaiveDate::from_ymd_opt(civil.year, civil.month, civil.day)?.and_hms_opt(
        civil.hour,
        civil.minute,
        0,
    )
}

/// The local wall-clock minute an instant falls in.
///
/// An instant always names exactly one local time - the ambiguity only runs
/// the other way - so `earliest` here is a formality, not a choice.
pub fn civil_from_epoch(epoch: i64) -> Option<CivilTime> {
    let at = Local.timestamp_opt(epoch, 0).earliest()?;
    CivilTime::new(at.year(), at.month(), at.day(), at.hour(), at.minute())
}

/// The first instant a cron expression fires strictly after `after`.
pub fn next_cron_after(expr: CronExpr, after: i64) -> Option<i64> {
    next_cron_after_with(expr, after, local_lookup)
}

fn next_cron_after_with<F>(expr: CronExpr, after: i64, lookup: F) -> Option<i64>
where
    F: Fn(NaiveDateTime) -> LocalResult<DateTime<Local>> + Copy,
{
    let mut from = civil_from_epoch(after)?;
    // A gap can push the resolved instant back to at or before `after` - the
    // slot was skipped over by the clock jump. Step past it rather than
    // returning a time that is not in the future.
    for _ in 0..GAP_RETRIES {
        let next = expr.next_after(from)?;
        let naive = naive_from_civil(next)?;
        let epoch = resolve_local(naive, lookup)?;
        if epoch > after {
            return Some(epoch);
        }
        from = next;
    }
    None
}

/// How many consecutive matches may resolve to a non-future instant before
/// giving up. Only reachable inside a DST gap, which is at most an hour of
/// minute-granular slots.
const GAP_RETRIES: usize = 120;

/// The next time this schedule fires after `from`, or `None` when nothing on
/// the wall clock ever will.
pub fn next_run_at(schedule: Schedule, from: i64) -> Option<i64> {
    match schedule {
        Schedule::Every(interval) => from.checked_add(interval.secs() as i64),
        Schedule::Cron(expr) => next_cron_after(expr, from),
        // Both are fired by something other than the clock, so they carry a
        // NULL `next_run_at` and the due query never sees them.
        Schedule::AtStartup | Schedule::Manual => None,
    }
}

/// Where `next_run_at` goes after a run is claimed.
///
/// The catch-up rule is the whole point. Advancing one slot at a time is
/// correct while the machine is awake and catastrophic when it is not: a
/// five-minute job across an eight-hour sleep would find itself due ninety-six
/// times over and fire in a burst. Anything older than `catchup_secs` is
/// collapsed into the single run that just happened, and the next slot is
/// computed from the edge of that window instead.
pub fn advance(schedule: Schedule, from: i64, now: i64, catchup_secs: i64) -> Option<i64> {
    let floor = now.saturating_sub(catchup_secs.max(0));
    let next = next_run_at(schedule, from)?;
    if next > floor {
        return Some(next);
    }
    // The backlog collapses into the run that just happened, but the *next*
    // slot still has to be a real next slot: `next_run_at(schedule, floor)`
    // only promises a result after `floor`, which for a short interval (or a
    // fine-grained cron expression) can still be at or before `now`. Stepping
    // forward from there guarantees the row is not already due again the
    // moment it is written - the difference between one catch-up run and a
    // burst that fires every scan until the clock finally catches up.
    let mut candidate = next_run_at(schedule, floor)?;
    while candidate <= now {
        candidate = next_run_at(schedule, candidate)?;
    }
    Some(candidate)
}
