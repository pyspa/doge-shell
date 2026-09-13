//! `rustup`/`cargo` value parsers: component/target names (stripped of the
//! shared host-triple suffix `rustup` prints but does not accept back), and
//! installed binary crate names.
use super::*;
use std::collections::BTreeMap;

/// `rustup component list` prints every component suffixed with the host
/// triple (`clippy-x86_64-unknown-linux-gnu`), but `rustup component add`
/// accepts the bare name. Strip the suffix shared by every entry.
pub(super) fn parse_rustup_components(lines: &[String]) -> Vec<String> {
    let names = rustup_listing_names(lines);
    let Some(host) = shared_target_triple(&names) else {
        return dedup_sorted(names);
    };
    let suffix = format!("-{host}");
    dedup_sorted(
        names
            .iter()
            .map(|name| {
                name.strip_suffix(&suffix)
                    .filter(|stripped| !stripped.is_empty())
                    .unwrap_or(name)
                    .to_string()
            })
            .collect(),
    )
}

pub(super) fn parse_rustup_targets(lines: &[String]) -> Vec<String> {
    dedup_sorted(rustup_listing_names(lines))
}

pub(super) fn rustup_listing_names(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| !name.is_empty() && !name.starts_with('('))
        .map(str::to_string)
        .collect()
}

/// Number of dash separated segments a target triple can span
/// (`aarch64-apple-darwin` through `armv7-unknown-linux-gnueabihf`).
pub(super) const TARGET_TRIPLE_MIN_SEGMENTS: usize = 3;
pub(super) const TARGET_TRIPLE_MAX_SEGMENTS: usize = 4;
/// How many components must end with a suffix before it is treated as the host
/// triple. Every non-host target contributes exactly one `rust-std-<triple>`
/// entry, so a threshold of two rules them out.
pub(super) const HOST_TRIPLE_MIN_OCCURRENCES: usize = 2;

/// Returns the host target triple that `rustup component list` appends to
/// component names. The listing also carries one `rust-std-<triple>` row per
/// supported target, so no suffix is common to *every* name; the host triple is
/// instead the one shared by the locally installable components (`cargo`,
/// `clippy`, `rust-src`, ...).
///
/// Longer suffixes are preferred because a three segment suffix of a four
/// segment triple (`unknown-linux-gnu` inside `x86_64-unknown-linux-gnu`) is
/// necessarily more frequent while being the wrong answer.
pub(super) fn shared_target_triple(names: &[String]) -> Option<String> {
    let segmented = names
        .iter()
        .map(|name| name.split('-').collect::<Vec<_>>())
        .collect::<Vec<_>>();

    for length in (TARGET_TRIPLE_MIN_SEGMENTS..=TARGET_TRIPLE_MAX_SEGMENTS).rev() {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for parts in &segmented {
            // Require a segment to survive stripping, so `cargo-<triple>` counts
            // but a bare triple does not.
            if parts.len() <= length {
                continue;
            }
            *counts
                .entry(parts[parts.len() - length..].join("-"))
                .or_default() += 1;
        }
        if let Some((suffix, _)) = counts
            .into_iter()
            .filter(|(_, count)| *count >= HOST_TRIPLE_MIN_OCCURRENCES)
            .max_by_key(|(_, count)| *count)
        {
            return Some(suffix);
        }
    }
    None
}

/// Parses `cargo install --list`, whose crate headers carry the version and end
/// with a colon (`ripgrep v14.1.0:`) while the binaries they installed follow on
/// their own lines. Indentation cannot be used to tell them apart because the
/// command runner trims every line before the parser sees it.
pub(super) fn parse_cargo_installed_crates(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter(|line| line.trim_end().ends_with(':'))
            .filter_map(|line| line.split_whitespace().next())
            .map(|name| name.trim_end_matches(':'))
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect(),
    )
}
