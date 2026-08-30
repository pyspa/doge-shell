//! `{{name}}` / `{{name:default}}` markers.
//!
//! Two features share this syntax: snippet expansion, which inserts the
//! defaults and leaves the cursor cycling between the stops (`dsh`), and
//! runbook playback, which prompts for each name (`dsh-builtin`). The scanner
//! lives here so the two cannot disagree about what counts as a placeholder —
//! they already did once, and Go-template syntax in a recorded command was
//! silently destroyed by the half that had no name validation.

/// A placeholder name and its optional default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placeholder {
    pub name: String,
    pub default: Option<String>,
}

/// One `{{...}}` occurrence, located in the source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marker<'a> {
    /// Byte offset of the opening `{{`.
    pub start: usize,
    /// Byte offset just past the closing `}}`.
    pub end: usize,
    pub name: &'a str,
    pub default: Option<&'a str>,
}

impl Marker<'_> {
    /// The default text, or the empty string when the marker carries none.
    pub fn default_text(&self) -> &str {
        self.default.unwrap_or("")
    }

    pub fn to_placeholder(self) -> Placeholder {
        Placeholder {
            name: self.name.to_string(),
            default: self.default.map(str::to_string),
        }
    }
}

/// Whether `{{...}}` content names a placeholder.
///
/// Templates legitimately carry `{{...}}` belonging to another language — the
/// `docker ps --format '{{json .}}'` this repo's own output schema injects,
/// Go templates, Helm, GitHub Actions. Substituting into those corrupts the
/// command, so only an identifier-shaped name counts.
pub fn is_placeholder_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

/// Every placeholder marker in `template`, in order.
///
/// Markers that are not placeholders — foreign template syntax, and an
/// unterminated `{{`, which ends the scan — are skipped, so a caller that
/// copies the text between markers reproduces them verbatim.
pub fn markers(template: &str) -> Vec<Marker<'_>> {
    let mut found = Vec::new();
    let mut offset = 0usize;

    while let Some(open) = template[offset..].find("{{") {
        let open = offset + open;
        let body = &template[open + 2..];
        let Some(close) = body.find("}}") else {
            break;
        };
        let end = open + 2 + close + 2;

        let inner = &body[..close];
        let (name, default) = match inner.split_once(':') {
            Some((name, default)) => (name, Some(default)),
            None => (inner, None),
        };
        if is_placeholder_name(name) {
            found.push(Marker {
                start: open,
                end,
                name,
                default,
            });
        }

        offset = end;
    }

    found
}

/// Unique placeholders in order of first appearance; the first default wins.
pub fn unique_placeholders(template: &str) -> Vec<Placeholder> {
    let mut found: Vec<Placeholder> = Vec::new();
    for marker in markers(template) {
        if !found.iter().any(|seen| seen.name == marker.name) {
            found.push(marker.to_placeholder());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_report_names_defaults_and_ranges() {
        let template = "scp {{src}} {{host:localhost}}:/tmp";
        let found = markers(template);

        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "src");
        assert_eq!(found[0].default, None);
        assert_eq!(&template[found[0].start..found[0].end], "{{src}}");

        assert_eq!(found[1].name, "host");
        assert_eq!(found[1].default, Some("localhost"));
        assert_eq!(
            &template[found[1].start..found[1].end],
            "{{host:localhost}}"
        );
    }

    #[test]
    fn foreign_template_syntax_is_not_a_placeholder() {
        for template in [
            "docker ps --format '{{json .}}'",
            "docker inspect --format '{{.State.Status}}' web",
            "helm template --set x={{ .Values.name }}",
            "echo {{}}",
            "echo {{2fast}}",
            "echo {{a b}}",
        ] {
            assert!(
                markers(template).is_empty(),
                "{template} has no placeholder"
            );
        }
    }

    #[test]
    fn an_unterminated_marker_ends_the_scan() {
        assert!(markers("echo {{oops").is_empty());
        // A valid marker before it is still reported.
        let found = markers("echo {{name}} then {{oops");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "name");
    }

    #[test]
    fn foreign_syntax_does_not_hide_a_later_placeholder() {
        let template = "docker ps --format '{{json .}}' | grep {{needle}}";
        let found = markers(template);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "needle");
        assert_eq!(&template[found[0].start..found[0].end], "{{needle}}");
    }

    #[test]
    fn unique_placeholders_deduplicate_keeping_the_first_default() {
        let found = unique_placeholders("{{host:a}} {{other}} {{host:b}}");
        assert_eq!(
            found,
            vec![
                Placeholder {
                    name: "host".into(),
                    default: Some("a".into())
                },
                Placeholder {
                    name: "other".into(),
                    default: None
                },
            ]
        );
    }

    #[test]
    fn placeholder_names_are_identifier_shaped() {
        assert!(is_placeholder_name("name"));
        assert!(is_placeholder_name("_private"));
        assert!(is_placeholder_name("with-dash"));
        assert!(is_placeholder_name("n2"));

        assert!(!is_placeholder_name(""));
        assert!(!is_placeholder_name("2fast"));
        assert!(!is_placeholder_name(".State"));
        assert!(!is_placeholder_name("json ."));
        assert!(!is_placeholder_name("日本語"));
    }
}
