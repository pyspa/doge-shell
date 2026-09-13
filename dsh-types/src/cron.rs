//! Five-field cron expressions, parsed and stepped without a calendar crate.
//!
//! `dsh-types` is the leaf every other crate depends on, so this module stays
//! dependency-free. [`CivilTime`] is a plain minute-precision wall-clock stamp
//! with no offset attached, and [`CronExpr::next_after`] walks one forward.
//! Turning that into a real instant - and deciding what a DST gap or overlap
//! means - is the shell's job and lives in `dsh/src/cron/clock.rs`, the only
//! place that touches `chrono`.
//!
//! The grammar is Vixie cron's, not Quartz's: five fields, no seconds column,
//! and none of `L` / `W` / `#` / `?`. Anything outside that is rejected with a
//! message rather than reinterpreted, because a `0 0 * * 0` copied out of a
//! crontab firing at the wrong time is the worst way this could fail.

pub mod job;
pub mod tool;

#[cfg(test)]
mod tests;

/// How far ahead [`CronExpr::next_after`] will look before giving up.
///
/// Eight years is the real floor, not a round number: `0 0 29 2 *` starting
/// from March 2096 next matches in 2104, because 2100 is not a leap year. Ten
/// leaves room without letting a never-matching expression spin.
const SEARCH_YEARS: i32 = 10;

const MINUTE_MAX: u32 = 59;
const HOUR_MAX: u32 = 23;
const DOM_MIN: u32 = 1;
const DOM_MAX: u32 = 31;
const MONTH_MIN: u32 = 1;
const MONTH_MAX: u32 = 12;
/// `7` and `0` both mean Sunday; [`normalise_dow`] folds 7 onto 0 after parsing.
const DOW_MAX: u32 = 7;

const MONTH_NAMES: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DOW_NAMES: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// A wall-clock stamp with no time zone, to the minute.
///
/// Deliberately not a `chrono` type: see the module doc. Ordering is
/// chronological, which is what [`CronExpr::next_after`] relies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CivilTime {
    pub year: i32,
    /// 1-12.
    pub month: u32,
    /// 1-31, valid for `month`.
    pub day: u32,
    /// 0-23.
    pub hour: u32,
    /// 0-59.
    pub minute: u32,
}

impl CivilTime {
    /// Returns `None` for a date that does not exist, such as 31 February.
    pub fn new(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> Option<Self> {
        if !(MONTH_MIN..=MONTH_MAX).contains(&month)
            || day < DOM_MIN
            || day > days_in_month(year, month)
            || hour > HOUR_MAX
            || minute > MINUTE_MAX
        {
            return None;
        }
        Some(Self {
            year,
            month,
            day,
            hour,
            minute,
        })
    }

    /// 0 = Sunday, matching the cron day-of-week field.
    pub fn weekday(self) -> u32 {
        // 1970-01-01 was a Thursday, so shift the epoch day into Sunday-first.
        (((days_from_civil(self.year, self.month, self.day) % 7) + 11) % 7) as u32
    }

    fn next_minute(self) -> Self {
        if self.minute < MINUTE_MAX {
            return Self {
                minute: self.minute + 1,
                ..self
            };
        }
        self.start_of_next_hour()
    }

    fn start_of_next_hour(self) -> Self {
        if self.hour < HOUR_MAX {
            return Self {
                hour: self.hour + 1,
                minute: 0,
                ..self
            };
        }
        self.start_of_next_day()
    }

    fn start_of_next_day(self) -> Self {
        if self.day < days_in_month(self.year, self.month) {
            return Self {
                day: self.day + 1,
                hour: 0,
                minute: 0,
                ..self
            };
        }
        self.start_of_next_month()
    }

    fn start_of_next_month(self) -> Self {
        let (year, month) = if self.month < MONTH_MAX {
            (self.year, self.month + 1)
        } else {
            (self.year.saturating_add(1), MONTH_MIN)
        };
        Self {
            year,
            month,
            day: 1,
            hour: 0,
            minute: 0,
        }
    }

    fn at_hour(self, hour: u32) -> Self {
        Self {
            hour,
            minute: 0,
            ..self
        }
    }
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        // Only reachable through a hand-built `CivilTime`; callers inside this
        // module always hold a validated month.
        _ => 0,
    }
}

/// Days since 1970-01-01, by Howard Hinnant's civil-date algorithm.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = i64::from(year - era * 400);
    let shifted_month = (i64::from(month) + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    i64::from(era) * 146_097 + day_of_era - 719_468
}

/// One parsed cron expression.
///
/// Every field is a bitmask, so the whole thing is `Copy` and can live inside
/// a `Copy` schedule enum. The original text is **not** kept: the store holds
/// the spelling the user typed, and [`CronExpr::canonical`] renders a
/// normalised form for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CronExpr {
    minute: u64,
    hour: u64,
    dom: u64,
    month: u64,
    dow: u64,
    /// Vixie's rule: a field counts as restricting only when it does not start
    /// with `*`. Both restricted means the two are OR'd, not AND'd.
    dom_restricted: bool,
    dow_restricted: bool,
}

struct FieldSpec {
    label: &'static str,
    min: u32,
    max: u32,
    names: &'static [&'static str],
    name_base: u32,
}

const MINUTE_SPEC: FieldSpec = FieldSpec {
    label: "minute",
    min: 0,
    max: MINUTE_MAX,
    names: &[],
    name_base: 0,
};
const HOUR_SPEC: FieldSpec = FieldSpec {
    label: "hour",
    min: 0,
    max: HOUR_MAX,
    names: &[],
    name_base: 0,
};
const DOM_SPEC: FieldSpec = FieldSpec {
    label: "day of month",
    min: DOM_MIN,
    max: DOM_MAX,
    names: &[],
    name_base: 0,
};
const MONTH_SPEC: FieldSpec = FieldSpec {
    label: "month",
    min: MONTH_MIN,
    max: MONTH_MAX,
    names: &MONTH_NAMES,
    name_base: MONTH_MIN,
};
const DOW_SPEC: FieldSpec = FieldSpec {
    label: "day of week",
    min: 0,
    max: DOW_MAX,
    names: &DOW_NAMES,
    name_base: 0,
};

/// Expands `@daily` and friends. `@reboot` is deliberately absent: it is not a
/// wall-clock rule, so the schedule layer above handles it as its own variant.
fn expand_macro(spec: &str) -> Option<&'static str> {
    Some(match spec.to_ascii_lowercase().as_str() {
        "@yearly" | "@annually" => "0 0 1 1 *",
        "@monthly" => "0 0 1 * *",
        "@weekly" => "0 0 * * 0",
        "@daily" | "@midnight" => "0 0 * * *",
        "@hourly" => "0 * * * *",
        _ => return None,
    })
}

/// Parses a five-field expression or one of the `@` macros.
pub fn parse_cron(spec: &str) -> Result<CronExpr, String> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err("empty schedule".to_string());
    }

    let expanded = if trimmed.starts_with('@') {
        expand_macro(trimmed).ok_or_else(|| {
            format!(
                "{trimmed}: unknown macro, expected @yearly, @annually, @monthly, @weekly, @daily, @midnight or @hourly"
            )
        })?
    } else {
        trimmed
    };

    let fields: Vec<&str> = expanded.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(format!(
            "{trimmed}: expected 5 fields (minute hour day-of-month month day-of-week), found {}",
            fields.len()
        ));
    }

    let (minute, _) = parse_field(fields[0], &MINUTE_SPEC)?;
    let (hour, _) = parse_field(fields[1], &HOUR_SPEC)?;
    let (dom, dom_star) = parse_field(fields[2], &DOM_SPEC)?;
    let (month, _) = parse_field(fields[3], &MONTH_SPEC)?;
    let (dow, dow_star) = parse_field(fields[4], &DOW_SPEC)?;

    Ok(CronExpr {
        minute,
        hour,
        dom,
        month,
        dow: normalise_dow(dow),
        dom_restricted: !dom_star,
        dow_restricted: !dow_star,
    })
}

/// Folds the `7` spelling of Sunday onto bit 0 so matching has one answer.
fn normalise_dow(mask: u64) -> u64 {
    if mask & (1 << DOW_MAX) != 0 {
        (mask | 1) & !(1 << DOW_MAX)
    } else {
        mask
    }
}

/// Returns the field's bitmask and whether it starts with `*`.
fn parse_field(field: &str, spec: &FieldSpec) -> Result<(u64, bool), String> {
    reject_unsupported(field, spec)?;
    let starts_with_star = field.starts_with('*');
    let mut mask = 0u64;

    for part in field.split(',') {
        if part.is_empty() {
            return Err(format!("{}: empty entry in `{field}`", spec.label));
        }

        let (range, step) = match part.split_once('/') {
            Some((range, step_text)) => {
                let step: u32 = step_text
                    .parse()
                    .map_err(|_| format!("{}: `{step_text}` is not a step number", spec.label))?;
                if step == 0 {
                    return Err(format!("{}: a step of 0 never matches", spec.label));
                }
                (range, step)
            }
            None => (part, 1),
        };

        let (low, high) = if range == "*" {
            (spec.min, spec.max)
        } else if let Some((start, end)) = range.split_once('-') {
            let start = parse_value(start, spec)?;
            let end = parse_value(end, spec)?;
            if start > end {
                return Err(format!(
                    "{}: range `{range}` starts after it ends",
                    spec.label
                ));
            }
            (start, end)
        } else {
            let value = parse_value(range, spec)?;
            if step != 1 {
                return Err(format!(
                    "{}: a step needs `*` or a range on its left, as in `{range}-{}/{step}`",
                    spec.label, spec.max
                ));
            }
            (value, value)
        };

        let mut value = low;
        while value <= high {
            mask |= 1 << value;
            value += step;
        }
    }

    if mask == 0 {
        return Err(format!("{}: `{field}` matches nothing", spec.label));
    }
    Ok((mask, starts_with_star))
}

/// Rejects Quartz-only syntax up front rather than letting it parse as
/// something else.
fn reject_unsupported(field: &str, spec: &FieldSpec) -> Result<(), String> {
    let upper = field.to_ascii_uppercase();
    if upper.contains('?') {
        return Err(format!("{}: `?` is Quartz syntax; use `*`", spec.label));
    }
    // `L`/`W` need to be checked name-aware: `jul` and `wed` are ordinary
    // month/weekday names that happen to contain those letters, not the
    // Quartz "last"/"nearest weekday" modifiers. `?` and `#` never appear
    // inside a real name, so they stay a plain substring check above and
    // below.
    if field_has_bare_marker(field, spec, 'L') {
        return Err(format!("{}: `L` (last) is not supported", spec.label));
    }
    if field_has_bare_marker(field, spec, 'W') {
        return Err(format!(
            "{}: `W` (nearest weekday) is not supported",
            spec.label
        ));
    }
    if upper.contains('#') {
        return Err(format!(
            "{}: `#` (nth weekday) is not supported",
            spec.label
        ));
    }
    Ok(())
}

/// Whether `marker` appears in `field` outside of a comma-separated part that
/// entirely spells one of this field's own recognised names (plainly, or as
/// a `-` range between two of them) - so `jul`, `wed`, or `mon-wed` are never
/// mistaken for the Quartz `L`/`W` modifiers merely because the name itself
/// contains that letter.
fn field_has_bare_marker(field: &str, spec: &FieldSpec, marker: char) -> bool {
    field.split(',').any(|part| {
        let is_name_expression = !spec.names.is_empty()
            && part.split('-').all(|piece| {
                spec.names
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(piece.trim()))
            });
        !is_name_expression && part.to_ascii_uppercase().contains(marker)
    })
}

fn parse_value(token: &str, spec: &FieldSpec) -> Result<u32, String> {
    let token = token.trim();
    if token.is_empty() {
        return Err(format!("{}: empty value", spec.label));
    }

    if !spec.names.is_empty() {
        let lowered = token.to_ascii_lowercase();
        if let Some(index) = spec.names.iter().position(|name| *name == lowered) {
            return Ok(index as u32 + spec.name_base);
        }
    }

    let value: u32 = token.parse().map_err(|_| {
        if spec.names.is_empty() {
            format!(
                "{}: `{token}` is not a number between {} and {}",
                spec.label, spec.min, spec.max
            )
        } else {
            format!(
                "{}: `{token}` is not a number between {} and {} or one of {}",
                spec.label,
                spec.min,
                spec.max,
                spec.names.join(", ")
            )
        }
    })?;

    if value < spec.min || value > spec.max {
        return Err(format!(
            "{}: {value} is outside {}-{}",
            spec.label, spec.min, spec.max
        ));
    }
    Ok(value)
}

fn bit_set(mask: u64, value: u32) -> bool {
    mask & (1 << value) != 0
}

/// The lowest set bit at or above `from`, within `max`.
fn next_set_bit(mask: u64, from: u32, max: u32) -> Option<u32> {
    (from..=max).find(|value| bit_set(mask, *value))
}

impl CronExpr {
    /// Whether this expression fires at `at`.
    pub fn matches(self, at: CivilTime) -> bool {
        bit_set(self.month, at.month)
            && self.day_matches(at)
            && bit_set(self.hour, at.hour)
            && bit_set(self.minute, at.minute)
    }

    /// Vixie's rule: when **both** day fields restrict, a day matching either
    /// one fires. `0 0 1 * mon` is "the 1st, and every Monday", not "Mondays
    /// that fall on the 1st".
    fn day_matches(self, at: CivilTime) -> bool {
        let by_dom = bit_set(self.dom, at.day);
        let by_dow = bit_set(self.dow, at.weekday());
        match (self.dom_restricted, self.dow_restricted) {
            (true, true) => by_dom || by_dow,
            (true, false) => by_dom,
            (false, true) => by_dow,
            (false, false) => true,
        }
    }

    /// The first match strictly after `after`, or `None` when the expression
    /// cannot match within [`SEARCH_YEARS`] - `0 0 30 2 *` never does.
    pub fn next_after(self, after: CivilTime) -> Option<CivilTime> {
        let limit = after.year.checked_add(SEARCH_YEARS)?;
        let mut at = after.next_minute();

        loop {
            if at.year > limit {
                return None;
            }
            if !bit_set(self.month, at.month) {
                at = at.start_of_next_month();
                continue;
            }
            if !self.day_matches(at) {
                at = at.start_of_next_day();
                continue;
            }
            if !bit_set(self.hour, at.hour) {
                at = match next_set_bit(self.hour, at.hour + 1, HOUR_MAX) {
                    Some(hour) => at.at_hour(hour),
                    None => at.start_of_next_day(),
                };
                continue;
            }
            match next_set_bit(self.minute, at.minute, MINUTE_MAX) {
                Some(minute) => {
                    at.minute = minute;
                    return Some(at);
                }
                None => at = at.start_of_next_hour(),
            }
        }
    }

    /// A normalised five-field rendering, for diagnostics. Not guaranteed to be
    /// the spelling the user typed - the store keeps that.
    pub fn canonical(self) -> String {
        format!(
            "{} {} {} {} {}",
            render_field(self.minute, 0, MINUTE_MAX),
            render_field(self.hour, 0, HOUR_MAX),
            render_field(self.dom, DOM_MIN, DOM_MAX),
            render_field(self.month, MONTH_MIN, MONTH_MAX),
            render_field(self.dow, 0, DOW_MAX - 1),
        )
    }
}

impl std::fmt::Display for CronExpr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.canonical())
    }
}

/// Collapses a bitmask back into `*`, `a`, `a-b` and comma lists.
fn render_field(mask: u64, min: u32, max: u32) -> String {
    let full: u64 = (min..=max).map(|value| 1u64 << value).sum();
    if mask & full == full {
        return "*".to_string();
    }

    let mut parts = Vec::new();
    let mut value = min;
    while value <= max {
        if !bit_set(mask, value) {
            value += 1;
            continue;
        }
        let start = value;
        while value < max && bit_set(mask, value + 1) {
            value += 1;
        }
        parts.push(if start == value {
            start.to_string()
        } else {
            format!("{start}-{value}")
        });
        value += 1;
    }
    parts.join(",")
}
