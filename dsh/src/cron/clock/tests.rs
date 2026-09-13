use super::*;
use dsh_types::cron::parse_cron;
use dsh_types::schedule::parse_schedule;

fn local_at(epoch: i64) -> DateTime<Local> {
    Local.timestamp_opt(epoch, 0).single().expect("instant")
}

fn naive(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(year, month, day)
        .unwrap()
        .and_hms_opt(hour, minute, 0)
        .unwrap()
}

/// The ordinary case: one local stamp, one instant.
#[test]
fn a_single_local_time_resolves_to_its_instant() {
    let at = naive(2024, 6, 1, 9, 30);
    let resolved = resolve_local(at, |_| LocalResult::Single(local_at(1_717_234_200)));
    assert_eq!(resolved, Some(1_717_234_200));
}

/// The autumn overlap. Firing on both instants would run one slot twice, and
/// the job cannot tell the difference - so take the first and never come back.
#[test]
fn an_ambiguous_local_time_takes_the_earlier_instant() {
    let at = naive(2024, 11, 3, 1, 30);
    let earlier = local_at(1_730_614_200);
    let later = local_at(1_730_617_800);
    let resolved = resolve_local(at, move |_| LocalResult::Ambiguous(earlier, later));
    assert_eq!(resolved, Some(earlier.timestamp()));
    assert!(earlier.timestamp() < later.timestamp(), "fixture ordering");
}

/// The spring gap. 02:30 does not exist on the day the clock jumps, and a
/// daily 02:30 job still has to run - at the first minute that does exist.
#[test]
fn a_local_time_inside_a_gap_lands_after_it() {
    let at = naive(2024, 3, 10, 2, 30);
    let after_gap = naive(2024, 3, 10, 3, 0);
    let resolved = resolve_local(at, move |candidate| {
        if candidate < after_gap {
            LocalResult::None
        } else {
            LocalResult::Single(local_at(1_710_061_200))
        }
    });
    assert_eq!(resolved, Some(1_710_061_200));
}

/// A zone that never resolves must end the search rather than spin.
#[test]
fn an_endless_gap_gives_up() {
    let at = naive(2024, 3, 10, 2, 30);
    assert_eq!(resolve_local(at, |_| LocalResult::None), None);
}

/// An interval schedule is pure arithmetic - no calendar, no zone.
#[test]
fn an_interval_schedule_adds_its_seconds() {
    let five_minutes = parse_schedule("5m").unwrap();
    assert_eq!(next_run_at(five_minutes, 1_000), Some(1_300));
    assert_eq!(next_run_at(parse_schedule("30s").unwrap(), 0), Some(30));
}

/// Neither is fired by the clock, so both must report "no next time" and stay
/// out of the due query entirely.
#[test]
fn startup_and_manual_schedules_have_no_next_time() {
    assert_eq!(next_run_at(parse_schedule("@reboot").unwrap(), 1_000), None);
    assert_eq!(next_run_at(parse_schedule("@manual").unwrap(), 1_000), None);
}

/// Asserted as a property rather than a fixed instant: these must hold in
/// whatever zone the test host is configured for.
#[test]
fn a_cron_schedule_lands_in_the_future() {
    let now = 1_717_234_200;
    for (spec, within) in [
        ("* * * * *", 120),
        ("*/5 * * * *", 600),
        ("@hourly", 2 * 3600),
        ("@daily", 25 * 3600),
    ] {
        let next = next_cron_after(parse_cron(spec).unwrap(), now).expect(spec);
        assert!(next > now, "{spec}: {next} is not after {now}");
        assert!(
            next <= now + within,
            "{spec}: {next} is more than {within}s out"
        );
    }
}

#[test]
fn an_impossible_cron_schedule_has_no_next_time() {
    assert_eq!(
        next_cron_after(parse_cron("0 0 30 2 *").unwrap(), 1_717_234_200),
        None
    );
}

/// Stepping repeatedly must stay strictly increasing; a gap that resolved
/// backwards would otherwise wedge a job at one instant forever.
#[test]
fn stepping_a_cron_schedule_strictly_increases() {
    let expr = parse_cron("*/7 * * * *").unwrap();
    let mut at = 1_717_234_200;
    for _ in 0..50 {
        let next = next_cron_after(expr, at).expect("next");
        assert!(next > at, "{next} did not advance past {at}");
        at = next;
    }
}

/// While the machine is awake, advancing is just "the next slot".
#[test]
fn advance_takes_the_next_slot_when_nothing_was_missed() {
    let schedule = parse_schedule("5m").unwrap();
    // Due right now, one slot behind at most.
    assert_eq!(advance(schedule, 1_000, 1_000, 3_600), Some(1_300));
}

/// The case this rule exists for: a laptop asleep for eight hours owes
/// ninety-six runs of a five-minute job. It gets one.
#[test]
fn advance_collapses_a_backlog_into_a_single_slot() {
    let schedule = parse_schedule("5m").unwrap();
    let slept_at = 1_000;
    let woke_at = slept_at + 8 * 3_600;
    let next = advance(schedule, slept_at, woke_at, 3_600).unwrap();
    // Strictly after `now`, not merely after the catch-up window's floor: a
    // result that is still in the past relative to `woke_at` would be due
    // again on the very next scan, turning "one catch-up run" into a burst.
    assert!(
        next > woke_at,
        "{next} is not in the future and would fire again immediately"
    );
    assert!(
        next <= woke_at + 300,
        "{next} skipped past the next real slot"
    );
}

/// The exact shape of the bug this guards against: a short interval inside a
/// long catch-up window used to make `next_run_at(schedule, floor)` land
/// before `now`, so the job would be claimed again immediately on the next
/// scan and fire repeatedly until it finally caught up one interval at a
/// time - a burst, not the single catch-up run the design promises.
#[test]
fn advance_never_returns_a_slot_that_is_already_due() {
    let schedule = parse_schedule("5m").unwrap();
    let slept_at = 1_000;
    // A gap much larger than the catch-up window, with an interval far
    // shorter than that window - exactly the ratio that used to burst-fire.
    let woke_at = slept_at + 10 * 3_600;
    let next = advance(schedule, slept_at, woke_at, 3_600).unwrap();
    assert!(next > woke_at, "{next} is due at or before {woke_at}");
}

/// A zero window means "never catch up": the next slot is measured from now.
#[test]
fn advance_with_no_catchup_window_measures_from_now() {
    let schedule = parse_schedule("5m").unwrap();
    let next = advance(schedule, 1_000, 100_000, 0).unwrap();
    assert_eq!(next, 100_300);
}

/// A negative window is a corrupt row, not a licence to fire in a loop.
#[test]
fn advance_treats_a_negative_window_as_zero() {
    let schedule = parse_schedule("5m").unwrap();
    assert_eq!(advance(schedule, 1_000, 100_000, -5), Some(100_300));
}

#[test]
fn advance_has_no_next_slot_for_a_non_clock_schedule() {
    let schedule = parse_schedule("@manual").unwrap();
    assert_eq!(advance(schedule, 1_000, 1_000, 3_600), None);
}

/// Round-tripping an instant through civil time must land on the same minute.
#[test]
fn civil_time_round_trips_through_the_local_zone() {
    for epoch in [0, 1_717_234_200, 1_730_614_200, 2_000_000_000] {
        let civil = civil_from_epoch(epoch).expect("civil");
        let resolved = resolve_local(
            naive(civil.year, civil.month, civil.day, civil.hour, civil.minute),
            local_lookup,
        )
        .expect("resolved");
        // Equal to the minute: the civil stamp drops seconds.
        assert_eq!(resolved, epoch - epoch.rem_euclid(60), "{epoch}");
    }
}
