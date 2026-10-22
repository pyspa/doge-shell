use std::collections::HashMap;

/// The exact shell-exported variables unscoped AI child processes may inherit.
/// This is intentionally local to AI tools; ordinary shell children keep the
/// full exported environment.
const UNSCOPED_AI_CHILD_ENV_ALLOWLIST: [&str; 5] = ["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR"];

pub(super) fn filter_unscoped_ai_child_environment(
    mut child_environment: HashMap<String, String>,
) -> HashMap<String, String> {
    child_environment.retain(|name, _| UNSCOPED_AI_CHILD_ENV_ALLOWLIST.contains(&name.as_str()));
    child_environment
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_uses_only_exact_exported_entries() {
        let filtered = filter_unscoped_ai_child_environment(HashMap::from([
            ("PATH".into(), "/logical/bin".into()),
            ("LC_CTYPE".into(), "C.UTF-8".into()),
            ("XDG_CONFIG_HOME".into(), "/logical/config".into()),
            (
                "DOGESH_F1_PROVIDER_KEY".into(),
                "synthetic-provider-value".into(),
            ),
        ]));

        assert_eq!(
            filtered,
            HashMap::from([("PATH".into(), "/logical/bin".into())])
        );

        // An absent exported PATH stays absent: this helper has no ambient
        // process-environment fallback.
        assert!(filter_unscoped_ai_child_environment(HashMap::new()).is_empty());
    }
}
