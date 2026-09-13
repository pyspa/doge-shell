use super::*;

fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> CivilTime {
    CivilTime::new(year, month, day, hour, minute).expect("test date must be valid")
}

fn next(spec: &str, from: CivilTime) -> Option<CivilTime> {
    parse_cron(spec).expect("spec must parse").next_after(from)
}

#[test]
fn every_macro_expands() {
    for (macro_spec, equivalent) in [
        ("@yearly", "0 0 1 1 *"),
        ("@annually", "0 0 1 1 *"),
        ("@monthly", "0 0 1 * *"),
        ("@weekly", "0 0 * * 0"),
        ("@daily", "0 0 * * *"),
        ("@midnight", "0 0 * * *"),
        ("@hourly", "0 * * * *"),
        ("@DAILY", "0 0 * * *"),
    ] {
        assert_eq!(
            parse_cron(macro_spec).unwrap(),
            parse_cron(equivalent).unwrap(),
            "{macro_spec}"
        );
    }
}

#[test]
fn reboot_is_not_a_wall_clock_expression() {
    // The schedule layer owns `@reboot`; this parser must not invent a time
    // for it.
    assert!(parse_cron("@reboot").is_err());
}

#[test]
fn accepts_the_whole_vixie_grammar() {
    for spec in [
        "* * * * *",
        "0 0 * * *",
        "*/5 * * * *",
        "0 9-17 * * 1-5",
        "0,30 * * * *",
        "0 0-23/2 * * *",
        "0 0 1,15 * *",
        "0 0 * jan,dec *",
        "0 0 * * sun",
        "0 0 * * SAT",
        "0 0 * * 7",
        "  0 0 * * *  ",
        "0 0 * jul *",
        "0 0 * * wed",
        "0 0 * * mon-wed",
    ] {
        assert!(parse_cron(spec).is_ok(), "{spec} should parse");
    }
}

#[test]
fn rejects_syntax_it_would_otherwise_misread() {
    // Every one of these parses as something else in at least one other cron
    // dialect, so silence would be the dangerous outcome.
    for spec in [
        "",
        "0 0 * *",
        "0 0 * * * *",
        "0 0 * * mon#1",
        "0 0 L * *",
        "0 0 1W * *",
        "0 0 * * ?",
        "60 0 * * *",
        "0 24 * * *",
        "0 0 0 * *",
        "0 0 32 * *",
        "0 0 * 13 *",
        "0 0 * * 8",
        "0 0 * * monday",
        "*/0 * * * *",
        "5-1 * * * *",
        "5/2 * * * *",
        "0 0 * * ,",
        "@nightly",
    ] {
        assert!(parse_cron(spec).is_err(), "{spec} should be rejected");
    }
}

/// `jul` and `wed` are ordinary names, not the Quartz `L`/`W` modifiers that
/// happen to be substrings of their uppercased spelling - `reject_unsupported`
/// used to uppercase the whole field and check for a bare `L`/`W` before ever
/// trying a name lookup, so both of these were rejected outright.
#[test]
fn month_and_weekday_names_containing_l_or_w_are_not_mistaken_for_quartz_syntax() {
    assert!(parse_cron("0 0 * jul *").is_ok());
    assert!(parse_cron("0 0 * * wed").is_ok());
    assert!(parse_cron("0 0 * * mon-wed").is_ok());
    // True Quartz syntax must still be rejected, including on these same
    // fields: a bare `L`/`W` is never a valid month or weekday name.
    assert!(parse_cron("0 0 * L *").is_err());
    assert!(parse_cron("0 0 * * 1W").is_err());
}

/// A step with a bare value on its left is a Quartz-ism. Rejecting it is only
/// useful if the message says what to write instead.
#[test]
fn a_bare_step_says_how_to_fix_it() {
    let error = parse_cron("5/2 * * * *").unwrap_err();
    assert!(error.contains("5-59/2"), "{error}");
}

#[test]
fn sunday_is_the_same_day_spelled_0_or_7() {
    let zero = parse_cron("0 0 * * 0").unwrap();
    let seven = parse_cron("0 0 * * 7").unwrap();
    assert_eq!(zero, seven);
    assert_eq!(seven.canonical(), "0 0 * * 0");
}

/// 1970-01-01 was a Thursday. Everything else in this module hangs off that.
#[test]
fn weekday_is_sunday_first_and_anchored_to_the_epoch() {
    assert_eq!(at(1970, 1, 1, 0, 0).weekday(), 4);
    assert_eq!(at(1970, 1, 4, 0, 0).weekday(), 0);
    assert_eq!(at(2024, 2, 29, 0, 0).weekday(), 4);
    assert_eq!(at(2000, 1, 1, 0, 0).weekday(), 6);
    assert_eq!(at(1969, 12, 31, 0, 0).weekday(), 3);
    assert_eq!(at(1900, 1, 1, 0, 0).weekday(), 1);
}

#[test]
fn steps_and_ranges_select_the_values_they_name() {
    let expr = parse_cron("0,15,30,45 * * * *").unwrap();
    for minute in 0..=59 {
        assert_eq!(
            expr.matches(at(2024, 1, 1, 0, minute)),
            minute % 15 == 0,
            "minute {minute}"
        );
    }
}

/// The rule people get wrong most often: two restricted day fields are OR'd.
#[test]
fn both_day_fields_restricted_means_or() {
    // 2024-03-01 is a Friday; the 1st and every Monday both fire.
    let expr = parse_cron("0 0 1 * mon").unwrap();
    assert!(expr.matches(at(2024, 3, 1, 0, 0)), "the 1st, not a Monday");
    assert!(expr.matches(at(2024, 3, 4, 0, 0)), "a Monday, not the 1st");
    assert!(!expr.matches(at(2024, 3, 5, 0, 0)), "neither");
}

#[test]
fn one_restricted_day_field_still_restricts() {
    let by_dom = parse_cron("0 0 1 * *").unwrap();
    assert!(by_dom.matches(at(2024, 3, 1, 0, 0)));
    assert!(!by_dom.matches(at(2024, 3, 4, 0, 0)));

    let by_dow = parse_cron("0 0 * * mon").unwrap();
    assert!(by_dow.matches(at(2024, 3, 4, 0, 0)));
    assert!(!by_dow.matches(at(2024, 3, 1, 0, 0)));
}

/// `*/1` is still a `*`, so it must not turn the other day field into an OR.
#[test]
fn a_starred_step_does_not_restrict() {
    let expr = parse_cron("0 0 1 * */1").unwrap();
    assert!(expr.matches(at(2024, 3, 1, 0, 0)));
    assert!(
        !expr.matches(at(2024, 3, 4, 0, 0)),
        "*/1 must not add an OR arm"
    );
}

#[test]
fn next_after_is_strictly_after() {
    let noon = at(2024, 3, 1, 12, 0);
    assert_eq!(next("0 12 * * *", noon), Some(at(2024, 3, 2, 12, 0)));
}

#[test]
fn next_after_rolls_up_every_field() {
    for (spec, from, expected) in [
        ("*/5 * * * *", at(2024, 3, 1, 10, 2), at(2024, 3, 1, 10, 5)),
        ("0 * * * *", at(2024, 3, 1, 10, 30), at(2024, 3, 1, 11, 0)),
        ("30 9 * * *", at(2024, 3, 1, 10, 0), at(2024, 3, 2, 9, 30)),
        ("0 0 1 * *", at(2024, 3, 2, 0, 0), at(2024, 4, 1, 0, 0)),
        ("0 0 1 1 *", at(2024, 3, 2, 0, 0), at(2025, 1, 1, 0, 0)),
        // End-of-month rollover, including the short month.
        ("0 0 * * *", at(2024, 2, 29, 0, 0), at(2024, 3, 1, 0, 0)),
        ("0 0 * * *", at(2023, 2, 28, 0, 0), at(2023, 3, 1, 0, 0)),
        ("0 0 * * *", at(2024, 12, 31, 0, 0), at(2025, 1, 1, 0, 0)),
        // Weekdays only: Friday's next run is Monday.
        ("0 9 * * 1-5", at(2024, 3, 1, 9, 0), at(2024, 3, 4, 9, 0)),
    ] {
        assert_eq!(next(spec, from), Some(expected), "{spec} from {from:?}");
    }
}

#[test]
fn leap_day_skips_to_the_next_leap_year() {
    assert_eq!(
        next("0 0 29 2 *", at(2024, 3, 1, 0, 0)),
        Some(at(2028, 2, 29, 0, 0))
    );
}

/// 2100 is not a leap year, so this gap is eight years - longer than a naive
/// four-year search window would cover.
#[test]
fn leap_day_crosses_the_non_leap_century() {
    assert_eq!(
        next("0 0 29 2 *", at(2096, 3, 1, 0, 0)),
        Some(at(2104, 2, 29, 0, 0))
    );
}

#[test]
fn an_impossible_date_terminates_instead_of_spinning() {
    assert_eq!(next("0 0 30 2 *", at(2024, 1, 1, 0, 0)), None);
    assert_eq!(next("0 0 31 4 *", at(2024, 1, 1, 0, 0)), None);
}

#[test]
fn next_after_does_not_overflow_at_the_end_of_time() {
    let expr = parse_cron("0 0 * * *").unwrap();
    assert_eq!(expr.next_after(at(i32::MAX, 1, 1, 0, 0)), None);
}

#[test]
fn every_minute_steps_one_minute() {
    let expr = parse_cron("* * * * *").unwrap();
    let mut at_time = at(2024, 3, 1, 23, 58);
    at_time = expr.next_after(at_time).unwrap();
    assert_eq!(at_time, at(2024, 3, 1, 23, 59));
    at_time = expr.next_after(at_time).unwrap();
    assert_eq!(at_time, at(2024, 3, 2, 0, 0));
}

#[test]
fn every_value_next_after_produces_matches() {
    // Walking forward must only ever land on something `matches` agrees with.
    for spec in ["*/7 * * * *", "0 0 1,15 * *", "30 6 * * 2,4", "0 0 * feb *"] {
        let expr = parse_cron(spec).unwrap();
        let mut at_time = at(2024, 1, 1, 0, 0);
        for _ in 0..20 {
            at_time = expr.next_after(at_time).expect(spec);
            assert!(expr.matches(at_time), "{spec} produced {at_time:?}");
        }
    }
}

#[test]
fn canonical_round_trips() {
    for spec in [
        "* * * * *",
        "0 0 * * *",
        "0,15,30,45 * * * *",
        "0 9-17 * * 1-5",
        "0 0 1,15 * *",
    ] {
        let parsed = parse_cron(spec).unwrap();
        let rendered = parsed.canonical();
        assert_eq!(
            parse_cron(&rendered).unwrap(),
            parsed,
            "{spec} -> {rendered}"
        );
    }
}

#[test]
fn canonical_normalises_names_and_steps() {
    assert_eq!(
        parse_cron("*/30 * * * *").unwrap().canonical(),
        "0,30 * * * *"
    );
    assert_eq!(
        parse_cron("0 0 * jan,dec *").unwrap().canonical(),
        "0 0 * 1,12 *"
    );
    assert_eq!(
        parse_cron("0 0 * * mon-fri").unwrap().canonical(),
        "0 0 * * 1-5"
    );
    assert_eq!(parse_cron("@hourly").unwrap().to_string(), "0 * * * *");
}

#[test]
fn civil_time_rejects_dates_that_do_not_exist() {
    assert!(CivilTime::new(2023, 2, 29, 0, 0).is_none());
    assert!(CivilTime::new(2024, 2, 29, 0, 0).is_some());
    assert!(CivilTime::new(2024, 4, 31, 0, 0).is_none());
    assert!(CivilTime::new(2024, 13, 1, 0, 0).is_none());
    assert!(CivilTime::new(2024, 1, 0, 0, 0).is_none());
    assert!(CivilTime::new(2024, 1, 1, 24, 0).is_none());
    assert!(CivilTime::new(2024, 1, 1, 0, 60).is_none());
}

#[test]
fn civil_time_orders_chronologically() {
    assert!(at(2024, 1, 1, 0, 0) < at(2024, 1, 1, 0, 1));
    assert!(at(2024, 1, 1, 23, 59) < at(2024, 1, 2, 0, 0));
    assert!(at(2024, 12, 31, 23, 59) < at(2025, 1, 1, 0, 0));
}
