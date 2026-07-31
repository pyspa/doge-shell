use crate::completion::cache::CompletionCache;
use crate::completion::dynamic::DynamicCompletionProvider;
use crate::completion::integrated::{CandidateType, EnhancedCandidate, IntegratedCompletionEngine};
use crate::environment::Environment;
use crate::history::{History, HistoryQuery};
use crate::prompt::Prompt;
use crate::repl::Repl;
use crate::shell::Shell;
use parking_lot::Mutex as ParkingMutex;
use std::fs;
use std::hint::black_box;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub name: &'static str,
    pub iterations: usize,
    pub elapsed: Duration,
}

pub fn run_default_probes(iterations: usize) -> Vec<ProbeResult> {
    let iterations = iterations.max(1);
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime for latency probes");

    vec![
        probe_completion_cache_lookup(iterations),
        probe_dynamic_completion_cache_miss(),
        probe_dynamic_completion_cache_hit(iterations),
        probe_history_search(iterations),
        probe_history_search_large(iterations),
        runtime.block_on(probe_integrated_completion_git_subcommand_warm(iterations)),
        runtime.block_on(probe_integrated_completion_git_branch_cached(iterations)),
        runtime.block_on(probe_integrated_completion_docker_subcommand_warm(
            iterations,
        )),
        runtime.block_on(probe_integrated_completion_kubectl_subcommand_warm(
            iterations,
        )),
        runtime.block_on(probe_integrated_completion_static_json_warm(iterations)),
        runtime.block_on(probe_repl_analyze_input(iterations)),
        runtime.block_on(probe_repl_analyze_long_input(iterations)),
        runtime.block_on(probe_repl_analyze_quoted_path_input(iterations)),
        probe_prompt_render(iterations),
        runtime.block_on(probe_repl_print_input(iterations)),
        runtime.block_on(probe_repl_print_input_reanalyze(iterations)),
        runtime.block_on(probe_repl_print_input_with_suggestion(iterations)),
    ]
}

fn probe_dynamic_completion_cache_miss() -> ProbeResult {
    let environment = Environment::new();
    let provider = DynamicCompletionProvider::new(environment);
    let temp_dir = tempfile::tempdir().expect("dynamic completion probe temp dir");
    let scope_dir = temp_dir.path().join("miss");

    let elapsed = measure(1, || {
        let candidates = provider.collect_probe_cached_command_candidates(
            scope_dir.clone(),
            black_box("feat"),
            vec!["feature/probe".to_string()],
        );
        black_box(candidates.len());
    });

    ProbeResult {
        name: "dynamic_completion_cache_miss",
        iterations: 1,
        elapsed,
    }
}

fn probe_dynamic_completion_cache_hit(iterations: usize) -> ProbeResult {
    let environment = Environment::new();
    let provider = DynamicCompletionProvider::new(environment);
    let temp_dir = tempfile::tempdir().expect("dynamic completion probe temp dir");
    let scope_dir = temp_dir.path().join("hit");

    let _ = provider.collect_probe_cached_command_candidates(
        scope_dir.clone(),
        "feat",
        vec!["feature/probe".to_string()],
    );
    let start = Instant::now();
    loop {
        let candidates = provider.collect_probe_cached_command_candidates(
            scope_dir.clone(),
            "feat",
            vec!["feature/probe".to_string()],
        );
        if candidates
            .iter()
            .any(|candidate| candidate.text == "feature/probe")
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "timed out warming dynamic completion probe cache"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let elapsed = measure(iterations, || {
        let candidates = provider.collect_probe_cached_command_candidates(
            scope_dir.clone(),
            black_box("feat"),
            vec!["feature/probe".to_string()],
        );
        black_box(candidates.len());
    });

    ProbeResult {
        name: "dynamic_completion_cache_hit",
        iterations,
        elapsed,
    }
}

fn probe_completion_cache_lookup(iterations: usize) -> ProbeResult {
    let cache = CompletionCache::new(Duration::from_secs(60));
    for i in 0..1_024 {
        cache.set(
            format!("git checkout feature/{i:04}"),
            vec![enhanced_candidate(format!("feature/{i:04}"))],
        );
    }

    let elapsed = measure(iterations, || {
        let hit = cache.lookup(black_box("git checkout feature/0999"));
        black_box(hit.map(|lookup| lookup.candidates.len()));
    });

    ProbeResult {
        name: "completion_cache_lookup",
        iterations,
        elapsed,
    }
}

fn probe_history_search(iterations: usize) -> ProbeResult {
    let mut history = History::new();
    seed_history(&mut history, 10_000);

    let elapsed = measure(iterations, || {
        black_box(history.search_first(black_box("git checkout feature/99")));
        let query = HistoryQuery {
            text: Some("feature/99".to_string()),
            limit: Some(50),
            ..Default::default()
        };
        black_box(history.search_entries(&query).len());
    });

    ProbeResult {
        name: "history_search",
        iterations,
        elapsed,
    }
}

fn probe_history_search_large(iterations: usize) -> ProbeResult {
    let mut history = History::new();
    seed_history(&mut history, 50_000);

    let elapsed = measure(iterations, || {
        let query = HistoryQuery {
            text: Some("feature/499".to_string()),
            limit: Some(100),
            ..Default::default()
        };
        black_box(history.search_entries(&query).len());
    });

    ProbeResult {
        name: "history_search_large",
        iterations,
        elapsed,
    }
}

async fn probe_integrated_completion_git_subcommand_warm(iterations: usize) -> ProbeResult {
    let mut history = History::new();
    seed_history(&mut history, 2_000);
    let history = Arc::new(ParkingMutex::new(history));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let engine = completion_engine_without_path();
    let input = "git ch";

    let _ = engine
        .complete(input, input.chars().count(), &cwd, 64, Some(&history))
        .await;
    let start = Instant::now();
    for _ in 0..iterations {
        let result = engine
            .complete(
                black_box(input),
                input.chars().count(),
                &cwd,
                64,
                Some(&history),
            )
            .await;
        black_box(result.candidates.len());
    }
    let elapsed = start.elapsed();

    ProbeResult {
        name: "integrated_completion_git_subcommand_warm",
        iterations,
        elapsed,
    }
}

async fn probe_integrated_completion_git_branch_cached(iterations: usize) -> ProbeResult {
    let temp_dir = tempfile::tempdir().expect("integrated completion probe temp dir");
    let bin_dir = temp_dir.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("probe bin dir");
    write_executable_script(
        &bin_dir.join("git"),
        "#!/bin/sh\nif [ \"$1\" = \"for-each-ref\" ]; then printf 'feature/probe\\nfeature/cache\\nmain\\n'; fi\n",
    );

    let engine = completion_engine_with_paths(vec![bin_dir.display().to_string()]);
    let input = "git checkout feat";
    let _ = engine
        .complete(input, input.chars().count(), temp_dir.path(), 64, None)
        .await;

    let start = Instant::now();
    for _ in 0..iterations {
        let result = engine
            .complete(
                black_box(input),
                input.chars().count(),
                temp_dir.path(),
                64,
                None,
            )
            .await;
        black_box(result.candidates.len());
    }
    let elapsed = start.elapsed();

    ProbeResult {
        name: "integrated_completion_git_branch_cached",
        iterations,
        elapsed,
    }
}

async fn probe_integrated_completion_docker_subcommand_warm(iterations: usize) -> ProbeResult {
    probe_integrated_completion_warm_case(
        iterations,
        "integrated_completion_docker_subcommand_warm",
        "docker co",
    )
    .await
}

async fn probe_integrated_completion_kubectl_subcommand_warm(iterations: usize) -> ProbeResult {
    probe_integrated_completion_warm_case(
        iterations,
        "integrated_completion_kubectl_subcommand_warm",
        "kubectl ge",
    )
    .await
}

async fn probe_integrated_completion_static_json_warm(iterations: usize) -> ProbeResult {
    probe_integrated_completion_warm_case(
        iterations,
        "integrated_completion_static_json_warm",
        "rg --ig",
    )
    .await
}

async fn probe_integrated_completion_warm_case(
    iterations: usize,
    name: &'static str,
    input: &str,
) -> ProbeResult {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let engine = completion_engine_without_path();

    let _ = engine
        .complete(input, input.chars().count(), &cwd, 64, None)
        .await;
    let start = Instant::now();
    for _ in 0..iterations {
        let result = engine
            .complete(black_box(input), input.chars().count(), &cwd, 64, None)
            .await;
        black_box(result.candidates.len());
    }
    let elapsed = start.elapsed();

    ProbeResult {
        name,
        iterations,
        elapsed,
    }
}

fn probe_prompt_render(iterations: usize) -> ProbeResult {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut prompt = Prompt::new(cwd, "🐕 < ".to_string());

    let elapsed = measure(iterations, || {
        let mut out = Vec::with_capacity(256);
        prompt.print_preprompt(&mut out);
        black_box(out.len());
    });

    ProbeResult {
        name: "prompt_render",
        iterations,
        elapsed,
    }
}

async fn probe_repl_print_input(iterations: usize) -> ProbeResult {
    let environment = Environment::new();
    let mut shell = Shell::new(environment);
    shell.cmd_history = Some(Arc::new(ParkingMutex::new(History::new())));

    let mut repl = Repl::new(&mut shell);
    repl.columns = 120;
    repl.input.reset("git status --short".to_string());

    let elapsed = measure(iterations, || {
        let mut out = Vec::with_capacity(512);
        repl.print_input(&mut out, false, false);
        black_box(out.len());
    });

    // The probe never calls `setup`, so dropping Repl would only print terminal
    // teardown sequences and save empty timing/history state into the user's env.
    std::mem::forget(repl);

    ProbeResult {
        name: "repl_print_input",
        iterations,
        elapsed,
    }
}

/// Same as `probe_repl_print_input`, but with `refresh_suggestion = true` so the
/// ghost-text path (history prediction, integrated/dynamic completion lookups)
/// is included. The other `print_input` probes pass `false` and therefore skip
/// the most expensive part of a real keystroke.
async fn probe_repl_print_input_with_suggestion(iterations: usize) -> ProbeResult {
    let environment = Environment::new();
    let mut shell = Shell::new(environment);
    shell.cmd_history = Some(Arc::new(ParkingMutex::new(History::new())));

    let mut repl = Repl::new(&mut shell);
    repl.columns = 120;
    repl.input.reset("git status --short".to_string());

    let elapsed = measure(iterations, || {
        repl.last_analyzed_input.clear();
        repl.last_analysis_result = None;
        let mut out = Vec::with_capacity(512);
        repl.print_input(&mut out, false, true);
        black_box(out.len());
    });

    std::mem::forget(repl);

    ProbeResult {
        name: "repl_print_input_with_suggestion",
        iterations,
        elapsed,
    }
}

async fn probe_repl_print_input_reanalyze(iterations: usize) -> ProbeResult {
    let environment = Environment::new();
    let mut shell = Shell::new(environment);
    shell.cmd_history = Some(Arc::new(ParkingMutex::new(History::new())));

    let mut repl = Repl::new(&mut shell);
    repl.columns = 120;
    repl.input.reset("git status --short".to_string());

    let elapsed = measure(iterations, || {
        repl.last_analyzed_input.clear();
        repl.last_analysis_result = None;
        let mut out = Vec::with_capacity(512);
        repl.print_input(&mut out, false, false);
        black_box(out.len());
    });

    std::mem::forget(repl);

    ProbeResult {
        name: "repl_print_input_reanalyze",
        iterations,
        elapsed,
    }
}

async fn probe_repl_analyze_input(iterations: usize) -> ProbeResult {
    probe_repl_analyze_input_case(iterations, "repl_analyze_input", "git status --short").await
}

async fn probe_repl_analyze_long_input(iterations: usize) -> ProbeResult {
    probe_repl_analyze_input_case(
        iterations,
        "repl_analyze_long_input",
        "cargo test -p doge-shell completion::dynamic::tests::git_branch_cache_is_shared_per_project_root_and_expires_by_ttl -- --nocapture",
    )
    .await
}

async fn probe_repl_analyze_quoted_path_input(iterations: usize) -> ProbeResult {
    probe_repl_analyze_input_case(
        iterations,
        "repl_analyze_quoted_path_input",
        r#"rg "completion cache" "dsh/src/completion/dynamic.rs""#,
    )
    .await
}

async fn probe_repl_analyze_input_case(
    iterations: usize,
    name: &'static str,
    input: &str,
) -> ProbeResult {
    let environment = Environment::new();
    let mut shell = Shell::new(environment);
    shell.cmd_history = Some(Arc::new(ParkingMutex::new(History::new())));

    let mut repl = Repl::new(&mut shell);
    repl.columns = 120;
    repl.input.reset(input.to_string());

    let elapsed = measure(iterations, || {
        let analysis = repl.analyze_input(black_box(input), None);
        black_box((
            analysis.can_execute,
            analysis.completion.as_ref().map(String::len),
            analysis.color_ranges.as_ref().map(Vec::len),
        ));
    });

    std::mem::forget(repl);

    ProbeResult {
        name,
        iterations,
        elapsed,
    }
}

fn completion_engine_without_path() -> IntegratedCompletionEngine {
    completion_engine_with_paths(Vec::new())
}

fn completion_engine_with_paths(paths: Vec<String>) -> IntegratedCompletionEngine {
    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.paths = paths;
        env.clear_command_cache();
    }
    let mut engine = IntegratedCompletionEngine::new(environment);
    engine
        .initialize_command_completion()
        .expect("initialize command completion");
    engine
}

fn write_executable_script(path: &std::path::Path, content: &str) {
    fs::write(path, content).expect("write probe executable");
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path)
            .expect("probe executable metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("probe executable permissions");
    }
}

fn measure(iterations: usize, mut f: impl FnMut()) -> Duration {
    let start = Instant::now();
    for _ in 0..iterations {
        f();
    }
    start.elapsed()
}

fn enhanced_candidate(text: String) -> EnhancedCandidate {
    EnhancedCandidate {
        text,
        description: None,
        candidate_type: CandidateType::Argument,
        priority: 0,
    }
}

fn seed_history(history: &mut History, entries: usize) {
    let batch = (0..entries)
        .map(|i| {
            (
                format!("git checkout feature/{i:04}"),
                chrono::Local::now().timestamp() + i as i64,
            )
        })
        .collect();
    let _ = history.write_batch(batch);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_probes_include_dynamic_cache_and_run() {
        let results = run_default_probes(1);
        let names = results.iter().map(|result| result.name).collect::<Vec<_>>();

        assert!(names.contains(&"dynamic_completion_cache_miss"));
        assert!(names.contains(&"dynamic_completion_cache_hit"));
        assert!(names.contains(&"integrated_completion_git_subcommand_warm"));
        assert!(names.contains(&"integrated_completion_git_branch_cached"));
        assert!(names.contains(&"integrated_completion_docker_subcommand_warm"));
        assert!(names.contains(&"integrated_completion_kubectl_subcommand_warm"));
        assert!(names.contains(&"integrated_completion_static_json_warm"));
        assert!(results.iter().all(|result| result.iterations >= 1));
    }
}
