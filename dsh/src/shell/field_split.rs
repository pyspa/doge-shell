//! Pure IFS field splitting for runtime word expansion.
//!
//! This module owns shell field-splitting semantics: how an expanded word
//! becomes zero, one, or many fields. It is deliberately free of shell
//! state, spawning, and globbing so the state machine stays unit-testable.
//! Callers resolve [`IfsSpec`] once per word from `Environment`, build
//! [`ExpandedSegment`]s preserving split/glob provenance, then call
//! [`split_segments`]. Pathname expansion happens afterwards in
//! `word_expand`, never here.

/// How one argument-word expansion splits on IFS.
///
/// Resolution from the logical shell `Environment` (once per word):
/// `None` (unset) => [`IfsSpec::Default`] (space, tab, newline),
/// `Some("")` => [`IfsSpec::Disabled`], `Some(value)` => [`IfsSpec::Custom`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IfsSpec {
    Default,
    Disabled,
    Custom(String),
}

impl IfsSpec {
    /// Resolve from an already-looked-up `IFS` value.
    ///
    /// `None` means unset. The caller must pass the logical shell value;
    /// never read the process environment here.
    pub(crate) fn resolve(ifs_value: Option<&str>) -> Self {
        match ifs_value {
            None => Self::Default,
            Some("") => Self::Disabled,
            Some(value) => Self::Custom(value.to_string()),
        }
    }

    fn is_whitespace(&self, c: char) -> bool {
        match self {
            Self::Disabled => false,
            Self::Default => matches!(c, ' ' | '\t' | '\n'),
            Self::Custom(value) => matches!(c, ' ' | '\t' | '\n') && value.contains(c),
        }
    }

    fn is_non_whitespace(&self, c: char) -> bool {
        match self {
            Self::Disabled | Self::Default => false,
            Self::Custom(value) => value.contains(c) && !matches!(c, ' ' | '\t' | '\n'),
        }
    }
}

/// Whether characters from a segment may act as IFS delimiters.
///
/// Only unquoted parameter and command-substitution results are splittable.
/// Literal, escaped, single/double-quoted, and process-substitution text is
/// always protected, even when its character value occurs in `$IFS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SplitPolicy {
    Splittable,
    Protected,
}

/// Pathname/brace provenance for one segment.
///
/// - `Inactive`: quoted, escaped, or otherwise protected; pattern is fully
///   escaped and never matches.
/// - `DynamicGlob`: unquoted variable/command-substitution result. Glob
///   metacharacters (`* ? [`) stay active for pathname expansion, but braces
///   stay inactive so `{a,b}` from a value never retroactively expands.
/// - `Source`: literal source text with parser flags (`pattern_active` /
///   `brace_active`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PatternKind {
    Inactive,
    DynamicGlob,
    Source { glob: bool, brace: bool },
}

/// One provenance-tagged piece of an expanded word.
///
/// `word_expand` builds one of these per `WordPart` (after tilde/variable/
/// substitution resolution); [`split_segments`] streams over all of them so
/// a delimiter at an expansion edge still separates adjacent literal text.
#[derive(Debug, Clone)]
pub(crate) struct ExpandedSegment {
    pub text: String,
    pub pattern: String,
    pub split: SplitPolicy,
    pub pattern_kind: PatternKind,
    pub preserve_empty: bool,
    /// Logical boundary between positional arguments; independent of IFS.
    pub field_boundary_after: bool,
}

impl ExpandedSegment {
    pub(crate) fn protected(
        text: String,
        pattern: String,
        pattern_kind: PatternKind,
        preserve_empty: bool,
    ) -> Self {
        Self {
            text,
            pattern,
            split: SplitPolicy::Protected,
            pattern_kind,
            preserve_empty,
            field_boundary_after: false,
        }
    }

    pub(crate) fn splittable_dynamic(text: String, brace_escaped_pattern: String) -> Self {
        Self {
            text,
            pattern: brace_escaped_pattern,
            split: SplitPolicy::Splittable,
            pattern_kind: PatternKind::DynamicGlob,
            preserve_empty: false,
            field_boundary_after: false,
        }
    }
}

/// One field produced by [`split_segments`], before pathname expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SplitField {
    pub text: String,
    pub pattern: String,
    pub has_active_pattern: bool,
    pub has_brace: bool,
    pub preserve_empty: bool,
}

/// Split segments belonging to one `PlannedWord`.
///
/// Only characters from [`SplitPolicy::Splittable`] segments may delimit.
/// Non-whitespace IFS delimiters preserve interior empty fields
/// (`a::b` -> `a`, ``, `b`; `:a` -> ``, `a`) while a trailing delimiter
/// produces no extra field (`a:` -> `a`). Runs of IFS whitespace collapse
/// and leading/trailing whitespace-only separators are discarded, except
/// that an explicitly quoted empty anchors a field so adjacent delimiters
/// still split around it (`""$X` with `X=" b"` -> ``, `b`).
pub(crate) fn split_segments(segments: &[ExpandedSegment], ifs: &IfsSpec) -> Vec<SplitField> {
    // Flatten into ordered items so delimiter grouping can look across
    // segment edges. Protected blocks stay atomic: boundaries never occur
    // inside them.
    enum Item<'a> {
        Protected(&'a ExpandedSegment),
        SplittableChar(char),
        ExplicitMarker,
        FieldBoundary,
    }
    let mut items: Vec<Item<'_>> = Vec::new();
    // Splittable chars need brace-escaping per char for pattern building;
    // track which segment each char came from only via policy (all
    // splittable segments are DynamicGlob here).
    for seg in segments {
        if seg.text.is_empty() {
            if seg.preserve_empty {
                items.push(Item::ExplicitMarker);
            }
        } else {
            match seg.split {
                SplitPolicy::Protected => items.push(Item::Protected(seg)),
                SplitPolicy::Splittable => {
                    for ch in seg.text.chars() {
                        items.push(Item::SplittableChar(ch));
                    }
                }
            }
        }
        if seg.field_boundary_after {
            items.push(Item::FieldBoundary);
        }
    }

    let mut fields: Vec<SplitField> = Vec::new();
    let mut cur_text = String::new();
    let mut cur_pattern = String::new();
    let mut cur_glob = false;
    let mut cur_brace = false;
    let mut cur_explicit = false;

    // Push the current field unconditionally: callers have already decided
    // this boundary synthesizes a field (leading/interior non-whitespace
    // delimiters preserve empties; whitespace callers check first).
    // Every pushed field sets `preserve_empty` so downstream empty
    // filtering keeps delimiter-induced empties.
    let push_kept_field = |fields: &mut Vec<SplitField>,
                           cur_text: &mut String,
                           cur_pattern: &mut String,
                           cur_glob: &mut bool,
                           cur_brace: &mut bool,
                           cur_explicit: &mut bool| {
        fields.push(SplitField {
            text: std::mem::take(cur_text),
            pattern: std::mem::take(cur_pattern),
            has_active_pattern: std::mem::replace(cur_glob, false),
            has_brace: std::mem::replace(cur_brace, false),
            preserve_empty: true,
        });
        *cur_explicit = false;
    };

    let mut idx = 0;
    while idx < items.len() {
        match &items[idx] {
            Item::FieldBoundary => {
                if !cur_text.is_empty() || cur_explicit {
                    push_kept_field(
                        &mut fields,
                        &mut cur_text,
                        &mut cur_pattern,
                        &mut cur_glob,
                        &mut cur_brace,
                        &mut cur_explicit,
                    );
                }
                idx += 1;
            }
            Item::ExplicitMarker => {
                cur_explicit = true;
                idx += 1;
            }
            Item::Protected(seg) => {
                cur_text.push_str(&seg.text);
                cur_pattern.push_str(&seg.pattern);
                match seg.pattern_kind {
                    PatternKind::Inactive => {}
                    PatternKind::DynamicGlob => {
                        cur_glob = true;
                    }
                    PatternKind::Source { glob, brace } => {
                        cur_glob |= glob;
                        cur_brace |= brace;
                    }
                }
                cur_explicit |= seg.preserve_empty;
                idx += 1;
            }
            Item::SplittableChar(ch) => {
                let ch = *ch;
                if ifs.is_non_whitespace(ch) {
                    // Non-whitespace delimiter: always ends the current
                    // field, preserving leading/interior empties.
                    push_kept_field(
                        &mut fields,
                        &mut cur_text,
                        &mut cur_pattern,
                        &mut cur_glob,
                        &mut cur_brace,
                        &mut cur_explicit,
                    );
                    idx += 1;
                    // Adjacent IFS whitespace belongs to the same
                    // delimiter sequence.
                    while idx < items.len()
                        && matches!(items[idx], Item::SplittableChar(c) if ifs.is_whitespace(c))
                    {
                        idx += 1;
                    }
                    // Trailing delimiter produces no extra field: if we
                    // consumed to the end (only explicit markers may
                    // remain, which belong to an already-pushed context),
                    // leave the empty current unpushed. Explicit trailing
                    // markers after the delimiter are handled below when
                    // they set cur_explicit and the final push keeps them.
                    continue;
                }
                if ifs.is_whitespace(ch) {
                    // Look ahead past this whitespace run for a splittable
                    // non-whitespace delimiter: `a :b` with IFS=": " is one
                    // combined delimiter. Explicit markers block combining:
                    // a quoted empty between the two (`$A""$B`) stays its
                    // own field, so WS and NWS split separately there.
                    let mut look = idx;
                    while look < items.len()
                        && matches!(items[look], Item::SplittableChar(c) if ifs.is_whitespace(c))
                    {
                        look += 1;
                    }
                    let followed_by_nws = matches!(
                        items.get(look),
                        Some(Item::SplittableChar(c)) if ifs.is_non_whitespace(*c)
                    );
                    if followed_by_nws {
                        // One combined delimiter: push once and consume the
                        // whitespace run, the non-whitespace, and its
                        // trailing whitespace together.
                        push_kept_field(
                            &mut fields,
                            &mut cur_text,
                            &mut cur_pattern,
                            &mut cur_glob,
                            &mut cur_brace,
                            &mut cur_explicit,
                        );
                        // Consume whitespace run.
                        idx = look;
                        // Consume the non-whitespace delimiter itself.
                        if matches!(
                            items.get(idx),
                            Some(Item::SplittableChar(c)) if ifs.is_non_whitespace(*c)
                        ) {
                            idx += 1;
                        }
                        // Consume trailing whitespace of the same delimiter.
                        while idx < items.len()
                            && matches!(items[idx], Item::SplittableChar(c) if ifs.is_whitespace(c))
                        {
                            idx += 1;
                        }
                        continue;
                    }
                    // Plain whitespace delimiter: collapse the run, never
                    // synthesizing empties.
                    if !cur_text.is_empty() || cur_explicit {
                        push_kept_field(
                            &mut fields,
                            &mut cur_text,
                            &mut cur_pattern,
                            &mut cur_glob,
                            &mut cur_brace,
                            &mut cur_explicit,
                        );
                    }
                    // Skip the whole whitespace run (and any explicit
                    // markers directly inside it stay attached to the next
                    // field: consume markers as explicit on the fresh
                    // current).
                    idx = look;
                    while idx < items.len() && matches!(items[idx], Item::ExplicitMarker) {
                        cur_explicit = true;
                        idx += 1;
                    }
                    continue;
                }
                // Ordinary content character from a splittable segment:
                // literal for splitting, brace-escaped for pattern.
                cur_text.push(ch);
                if matches!(ch, '\\' | '{' | '}') {
                    cur_pattern.push('\\');
                }
                cur_pattern.push(ch);
                cur_glob = true;
                idx += 1;
            }
        }
    }

    if !cur_text.is_empty() || cur_explicit {
        fields.push(SplitField {
            text: std::mem::take(&mut cur_text),
            pattern: std::mem::take(&mut cur_pattern),
            has_active_pattern: std::mem::replace(&mut cur_glob, false),
            has_brace: std::mem::replace(&mut cur_brace, false),
            preserve_empty: true,
        });
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protected(text: &str) -> ExpandedSegment {
        ExpandedSegment::protected(
            text.to_string(),
            text.to_string(),
            PatternKind::Inactive,
            !text.is_empty(),
        )
    }

    fn splittable(text: &str) -> ExpandedSegment {
        let mut pattern = String::new();
        for ch in text.chars() {
            if matches!(ch, '\\' | '{' | '}') {
                pattern.push('\\');
            }
            pattern.push(ch);
        }
        ExpandedSegment::splittable_dynamic(text.to_string(), pattern)
    }

    fn explicit_empty() -> ExpandedSegment {
        ExpandedSegment::protected(String::new(), String::new(), PatternKind::Inactive, true)
    }

    fn implicit_empty() -> ExpandedSegment {
        ExpandedSegment::splittable_dynamic(String::new(), String::new())
    }

    fn texts(fields: &[SplitField]) -> Vec<&str> {
        fields.iter().map(|f| f.text.as_str()).collect()
    }

    #[test]
    fn default_whitespace_collapse_and_trim() {
        let fields = split_segments(&[splittable("  a  b  ")], &IfsSpec::Default);
        assert_eq!(texts(&fields), vec!["a", "b"]);
    }

    #[test]
    fn unset_ifs_behaves_as_default() {
        let spec = IfsSpec::resolve(None);
        assert_eq!(spec, IfsSpec::Default);
        let fields = split_segments(&[splittable("a\tb\nc")], &spec);
        assert_eq!(texts(&fields), vec!["a", "b", "c"]);
    }

    #[test]
    fn empty_ifs_disables_splitting() {
        let spec = IfsSpec::resolve(Some(""));
        assert_eq!(spec, IfsSpec::Disabled);
        let fields = split_segments(&[splittable("a b")], &spec);
        assert_eq!(texts(&fields), vec!["a b"]);
    }

    #[test]
    fn custom_whitespace_only_splits_on_space() {
        let spec = IfsSpec::resolve(Some(" "));
        let fields = split_segments(&[splittable("a b\tc")], &spec);
        // Tab is not in IFS, so it stays literal inside the second field.
        assert_eq!(texts(&fields), vec!["a", "b\tc"]);
    }

    #[test]
    fn leading_non_whitespace_delimiter_keeps_empty() {
        let spec = IfsSpec::resolve(Some(":"));
        let fields = split_segments(&[splittable(":a")], &spec);
        assert_eq!(texts(&fields), vec!["", "a"]);
    }

    #[test]
    fn trailing_non_whitespace_delimiter_produces_no_extra_field() {
        let spec = IfsSpec::resolve(Some(":"));
        let fields = split_segments(&[splittable("a:")], &spec);
        assert_eq!(texts(&fields), vec!["a"]);
    }

    #[test]
    fn adjacent_non_whitespace_delimiters_keep_interior_empty() {
        let spec = IfsSpec::resolve(Some(":"));
        let fields = split_segments(&[splittable("a::b")], &spec);
        assert_eq!(texts(&fields), vec!["a", "", "b"]);
    }

    #[test]
    fn double_delimiter_alone_gives_two_empties() {
        let spec = IfsSpec::resolve(Some(":"));
        let fields = split_segments(&[splittable("::")], &spec);
        assert_eq!(texts(&fields), vec!["", ""]);
    }

    #[test]
    fn single_delimiter_alone_gives_one_empty() {
        let spec = IfsSpec::resolve(Some(":"));
        let fields = split_segments(&[splittable(":")], &spec);
        assert_eq!(texts(&fields), vec![""]);
    }

    #[test]
    fn mixed_whitespace_around_non_whitespace_is_one_delimiter() {
        let spec = IfsSpec::resolve(Some(": "));
        // `a: :b` is `a`, delimiter, empty, delimiter, `b`.
        let fields = split_segments(&[splittable("a: :b")], &spec);
        assert_eq!(texts(&fields), vec!["a", "", "b"]);
        // `a :b` is one combined delimiter, no interior empty.
        let fields = split_segments(&[splittable("a :b")], &spec);
        assert_eq!(texts(&fields), vec!["a", "b"]);
    }

    #[test]
    fn literal_prefix_plus_split_expansion() {
        // pre$X with X=" a" -> "pre", "a".
        let fields = split_segments(&[protected("pre"), splittable(" a")], &IfsSpec::Default);
        assert_eq!(texts(&fields), vec!["pre", "a"]);
    }

    #[test]
    fn split_expansion_plus_literal_suffix() {
        // $Xpost with X="a " -> "a", "post".
        let fields = split_segments(&[splittable("a "), protected("post")], &IfsSpec::Default);
        assert_eq!(texts(&fields), vec!["a", "post"]);
    }

    #[test]
    fn split_boundary_across_two_expansions() {
        // $A$B with A="a " and B=" b" -> "a", "b".
        let fields = split_segments(&[splittable("a "), splittable(" b")], &IfsSpec::Default);
        assert_eq!(texts(&fields), vec!["a", "b"]);
    }

    #[test]
    fn literal_ifs_char_never_delimits() {
        let spec = IfsSpec::resolve(Some(":"));
        let fields = split_segments(&[protected("a:b")], &spec);
        assert_eq!(texts(&fields), vec!["a:b"]);
    }

    #[test]
    fn quoted_explicit_empty_is_preserved() {
        let fields = split_segments(&[explicit_empty()], &IfsSpec::Default);
        assert_eq!(texts(&fields), vec![""]);
    }

    #[test]
    fn implicit_unquoted_empty_is_removed() {
        let fields = split_segments(&[implicit_empty()], &IfsSpec::Default);
        assert!(fields.is_empty());
        let fields = split_segments(&[implicit_empty()], &IfsSpec::resolve(Some(":")));
        assert!(fields.is_empty());
    }

    #[test]
    fn whitespace_only_expansion_vanishes() {
        let fields = split_segments(&[splittable("   ")], &IfsSpec::Default);
        assert!(fields.is_empty());
    }

    #[test]
    fn quoted_empty_anchors_leading_whitespace_split() {
        // ""$X with X=" b" -> "", "b".
        let fields = split_segments(&[explicit_empty(), splittable(" b")], &IfsSpec::Default);
        assert_eq!(texts(&fields), vec!["", "b"]);
    }

    #[test]
    fn quoted_empty_anchors_trailing_whitespace_split() {
        // $X"" with X="a " -> "a", "".
        let fields = split_segments(&[splittable("a "), explicit_empty()], &IfsSpec::Default);
        assert_eq!(texts(&fields), vec!["a", ""]);
    }

    #[test]
    fn quoted_empty_between_delimiters_stays_own_field() {
        // `$A""$B` with A="a ", B=":b", IFS=": ": the quoted empty blocks
        // WS+NWS combining, yielding "a", "", "b" (verified against bash).
        let spec = IfsSpec::resolve(Some(": "));
        let fields = split_segments(
            &[splittable("a "), explicit_empty(), splittable(":b")],
            &spec,
        );
        assert_eq!(texts(&fields), vec!["a", "", "b"]);
    }

    #[test]
    fn multibyte_ifs_delimiter_is_char_oriented() {
        // Custom multibyte delimiter splits on the character, and multibyte
        // content is never split inside a UTF-8 sequence.
        let spec = IfsSpec::resolve(Some("é,"));
        assert_eq!(
            texts(&split_segments(&[splittable("aéb")], &spec)),
            vec!["a", "b"]
        );
        assert_eq!(
            texts(&split_segments(&[splittable("café x")], &IfsSpec::Default)),
            vec!["café", "x"]
        );
    }

    #[test]
    fn custom_comma_cases() {
        let spec = IfsSpec::resolve(Some(","));
        assert_eq!(
            texts(&split_segments(&[splittable("a,,b")], &spec)),
            vec!["a", "", "b"]
        );
        assert_eq!(
            texts(&split_segments(&[splittable(",a")], &spec)),
            vec!["", "a"]
        );
        assert_eq!(
            texts(&split_segments(&[splittable("a,")], &spec)),
            vec!["a"]
        );
    }

    #[test]
    fn dynamic_glob_sets_pattern_flag_without_brace() {
        let fields = split_segments(&[splittable("*.txt")], &IfsSpec::Default);
        assert_eq!(fields.len(), 1);
        assert!(fields[0].has_active_pattern);
        assert!(!fields[0].has_brace);
        assert_eq!(fields[0].pattern, "*.txt");
    }

    #[test]
    fn dynamic_braces_are_escaped() {
        let fields = split_segments(&[splittable("{a,b}")], &IfsSpec::Default);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].text, "{a,b}");
        assert_eq!(fields[0].pattern, "\\{a,b\\}");
    }
    #[test]
    fn intrinsic_boundaries_preserve_fields_prefix_suffix_and_empty_arguments() {
        for ifs in [
            IfsSpec::Default,
            IfsSpec::Disabled,
            IfsSpec::Custom(": ".into()),
        ] {
            let mut first = protected("a");
            first.field_boundary_after = true;
            assert_eq!(
                texts(&split_segments(&[first.clone(), protected("b")], &ifs)),
                ["a", "b"]
            );
            assert_eq!(
                texts(&split_segments(
                    &[protected("pre"), first, protected("b"), protected("post")],
                    &ifs
                )),
                ["prea", "bpost"]
            );
            let mut empty = explicit_empty();
            empty.field_boundary_after = true;
            assert_eq!(
                texts(&split_segments(&[empty, protected("b")], &ifs)),
                ["", "b"]
            );
        }
    }

    #[test]
    fn intrinsic_boundary_does_not_combine_ifs_delimiters() {
        let mut first = splittable("a ");
        first.field_boundary_after = true;
        assert_eq!(
            texts(&split_segments(
                &[first, splittable(":b")],
                &IfsSpec::Custom(": ".into())
            )),
            ["a", "", "b"]
        );
    }
}
