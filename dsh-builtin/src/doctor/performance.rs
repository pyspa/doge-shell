//! `doctor performance`: history/completion cache size, latency probes, and
//! command timing stats.
use crate::ShellProxy;
use dsh_types::Context;
use std::fs;
use std::path::PathBuf;

pub(super) const PERFORMANCE_TOP_DEFAULT: usize = 5;

pub(super) fn check_performance(ctx: &Context, proxy: &mut dyn ShellProxy, args: &[String]) {
    let top_limit = performance_top_limit(args);
    match proxy.command_history_len() {
        Some(count) => {
            let _ = ctx.write_stdout(&format!("ok history-loaded entries={count}"));
        }
        None => {
            let _ = ctx.write_stdout("skip history-loaded unavailable");
        }
    }

    match proxy.executable_cache_len() {
        Some(count) => {
            let _ = ctx.write_stdout(&format!("ok path-cache memory-entries={count}"));
        }
        None => {
            let _ = ctx.write_stdout("skip path-cache memory-unavailable");
        }
    }

    match executable_cache_file_info() {
        Some((path, count)) => {
            let _ = ctx.write_stdout(&format!(
                "ok path-cache-file {} entries={count}",
                path.display()
            ));
        }
        None => {
            let _ = ctx.write_stdout("skip path-cache-file missing");
        }
    }

    let completion_diagnostics = proxy.completion_diagnostics();
    if completion_diagnostics.is_empty() {
        let _ = ctx.write_stdout("skip completion-cache unavailable");
    } else {
        for line in completion_diagnostics {
            let _ = ctx.write_stdout(&format!("ok {line}"));
        }
    }

    let _ = ctx.write_stdout("ok timing-flush debounce interval=5s threshold=10");

    if performance_latency_enabled(args) {
        let iterations = performance_latency_iterations(args).unwrap_or(1_000);
        let lines = proxy.latency_probe_lines(iterations);
        if lines.is_empty() {
            let _ = ctx.write_stdout("skip latency-probes unavailable");
        } else {
            for line in &lines {
                let _ = ctx.write_stdout(&format!("ok {line}"));
            }
            if let Some((name, avg_ns)) = slowest_latency_probe(&lines) {
                let _ = ctx.write_stdout(&format!(
                    "ok latency-slowest probe={name} avg={avg_ns}ns focus={}",
                    latency_probe_focus(name)
                ));
            }
        }
    } else {
        let _ = ctx.write_stdout("skip latency-probes pass --latency to run");
    }

    let timing_file = crate::command_timing::get_timing_file_path();
    match timing_file
        .as_ref()
        .and_then(crate::command_timing::CommandTiming::load_from_file)
    {
        Some(timing) => {
            let _ = ctx.write_stdout(&format!("ok timing-entries {}", timing.stats.len()));
            let _ = ctx.write_stdout(&format!("ok timing-top limit={top_limit}"));

            let slowest = timing.top_slowest(top_limit);
            if slowest.is_empty() {
                let _ = ctx.write_stdout("skip slowest none");
            } else {
                for (index, stats) in slowest.into_iter().enumerate() {
                    if index == 0 {
                        let _ = ctx.write_stdout(&format!(
                            "ok slowest {} avg={} success={:.1}%",
                            stats.command,
                            crate::command_timing::format_duration(stats.average_duration_ms()),
                            stats.success_rate()
                        ));
                    } else {
                        let _ = ctx.write_stdout(&format!(
                            "ok slowest#{} {} avg={} success={:.1}%",
                            index + 1,
                            stats.command,
                            crate::command_timing::format_duration(stats.average_duration_ms()),
                            stats.success_rate()
                        ));
                    }
                }
            }

            let frequent = timing.top_frequent(top_limit);
            if frequent.is_empty() {
                let _ = ctx.write_stdout("skip frequent none");
            } else {
                for (index, stats) in frequent.into_iter().enumerate() {
                    if index == 0 {
                        let _ = ctx.write_stdout(&format!(
                            "ok frequent {} calls={}",
                            stats.command, stats.total_calls
                        ));
                    } else {
                        let _ = ctx.write_stdout(&format!(
                            "ok frequent#{} {} calls={}",
                            index + 1,
                            stats.command,
                            stats.total_calls
                        ));
                    }
                }
            }
        }
        None => {
            let display_path = timing_file
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let _ = ctx.write_stdout(&format!("warn timing missing {}", display_path));
        }
    }

    let skills_dir = crate::config_paths::skills_dir();
    if skills_dir.exists() {
        let count = fs::read_dir(&skills_dir)
            .map(|entries| entries.count())
            .unwrap_or(0);
        let _ = ctx.write_stdout(&format!(
            "ok skills-scan {} entries={count}",
            skills_dir.display()
        ));
    } else {
        let _ = ctx.write_stdout(&format!(
            "skip skills-scan missing {}",
            skills_dir.display()
        ));
    }
}

pub(super) fn performance_latency_enabled(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--latency")
}

pub(super) fn performance_latency_iterations(args: &[String]) -> Option<usize> {
    args.windows(2).find_map(|window| {
        if window[0] == "--latency-iters" {
            window[1].parse::<usize>().ok()
        } else {
            None
        }
    })
}

pub(super) fn performance_top_limit(args: &[String]) -> usize {
    let parsed = args
        .windows(2)
        .find_map(|window| {
            if window[0] == "--top" {
                window[1].parse::<usize>().ok()
            } else {
                None
            }
        })
        .or_else(|| {
            args.iter().find_map(|arg| {
                arg.strip_prefix("--top=")
                    .and_then(|value| value.parse::<usize>().ok())
            })
        });

    parsed
        .filter(|value| *value > 0)
        .unwrap_or(PERFORMANCE_TOP_DEFAULT)
}

pub(super) fn slowest_latency_probe(lines: &[String]) -> Option<(&str, u128)> {
    lines
        .iter()
        .filter_map(|line| latency_probe_name_and_avg(line))
        .max_by_key(|(_, avg_ns)| *avg_ns)
}

pub(super) fn latency_probe_name_and_avg(line: &str) -> Option<(&str, u128)> {
    let rest = line.strip_prefix("latency ")?;
    let (name, metrics) = rest.split_once(' ')?;
    let avg_ns = metrics
        .split_whitespace()
        .find_map(|field| field.strip_prefix("avg=")?.strip_suffix("ns"))?
        .parse::<u128>()
        .ok()?;
    Some((name, avg_ns))
}

pub(super) fn latency_probe_focus(name: &str) -> &'static str {
    if name.starts_with("integrated_completion") {
        "completion"
    } else if name.starts_with("repl_analyze") || name.starts_with("repl_print") {
        "repl"
    } else if name.starts_with("history") {
        "history"
    } else if name.contains("cache") {
        "cache"
    } else {
        "runtime"
    }
}

pub(super) fn executable_cache_file_info() -> Option<(PathBuf, usize)> {
    let dirs = xdg::BaseDirectories::with_prefix("dsh");
    let path = dirs.place_data_file("executable_names.json").ok()?;
    let contents = fs::read_to_string(&path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let count = value
        .get("names")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    Some((path, count))
}
