use super::*;
use crate::completion::shell_path::{
    format_candidates_for_token, format_path_for_token, normalize_path_token,
};
use skim::SkimItem;

#[tokio::test]
async fn pm_add_name_preserves_literal_path_and_query() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    let names = [
        "plain",
        "space dir",
        "界 dir",
        "quo\"te",
        "single'name",
        r"back\slash",
        "trail\\",
        "trail\\\\",
    ];
    for name in names {
        std::fs::create_dir(cwd.join(name)).unwrap();
    }
    let env = Environment::new_isolated();
    env.write()
        .set_shell_var("DOGESH_EXTERNAL_COMPLETER".into(), String::new());
    env.write()
        .set_shell_var("DOGESH_COMPLETION_FISH_FALLBACK".into(), "off".into());
    let mut engine = IntegratedCompletionEngine::new(env);
    engine.initialize_command_completion().unwrap();
    for name in names {
        for style in ["", "''", "\"\""] {
            for slash in ["", "/"] {
                let path = format!("{}{slash}", cwd.join(name).display());
                let path_source = format_path_for_token(&path, style);
                // The parser owns source tokens; the consumer decodes them once.
                let prefix: String = name.chars().take(name.chars().count() - 1).collect();
                for query in [
                    String::new(),
                    format_path_for_token(&prefix, style),
                    format_path_for_token(name, style),
                ] {
                    let input = format!("pm add {path_source} {query}");
                    for _ in 0..2 {
                        let result = engine
                            .complete(&input, input.chars().count(), &cwd, 50, None)
                            .await;
                        let candidates: Vec<_> = result
                            .candidates
                            .into_iter()
                            .filter(|c| c.description.as_deref() == Some("from path"))
                            .collect();
                        assert_eq!(
                            candidates
                                .iter()
                                .map(|c| c.text.as_str())
                                .collect::<Vec<_>>(),
                            vec![name],
                            "{input}"
                        );
                        assert_eq!(candidates[0].candidate_type, CandidateType::Argument);
                        assert_eq!(candidates[0].priority, 95);
                        let formatted = format_candidates_for_token(
                            engine.to_candidates(candidates),
                            Some(&query),
                        );
                        assert_eq!(formatted[0].get_display_name(), name);
                        assert_eq!(normalize_path_token(&formatted[0].output()), name);
                    }
                }
            }
        }
    }
    let input = format!("pm add {}/plain zzz-unrelated", cwd.display());
    let result = engine.complete(&input, input.len(), &cwd, 50, None).await;
    assert!(
        !result
            .candidates
            .iter()
            .any(|c| c.description.as_deref() == Some("from path"))
    );
}
