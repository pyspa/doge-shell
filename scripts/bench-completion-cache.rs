//! Isolated completion microbenchmarks; no product settings or external tools are run.
//! Build: cargo build --release -p doge-shell --lib
//! rustc --edition=2024 -O scripts/bench-completion-cache.rs --extern
//! doge_shell=target/release/libdoge_shell.rlib -L dependency=target/release/deps
//! -o /tmp/bench-completion-cache
//! Run with HOME and XDG directories pointing to a disposable directory.
use doge_shell::completion::cache::CompletionCache;
use doge_shell::completion::integrated::{CandidateType, EnhancedCandidate};
use doge_shell::completion::path::{is_path_cached, path_completion_path_sync};
use doge_shell::environment::{Environment, collect_executables};
use std::fs;
use std::hint::black_box;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SAMPLES: usize = 20;
fn batch(iterations: usize, f: &mut impl FnMut()) -> f64 {
    let start = Instant::now();
    for _ in 0..iterations {
        f();
    }
    start.elapsed().as_nanos() as f64 / iterations as f64
}
fn emit(case: &str, n: usize, iterations: usize, times: &mut [f64]) {
    times.sort_by(f64::total_cmp);
    println!(
        "{case},{n},{iterations},{SAMPLES},{:.1},{:.1}",
        times[SAMPLES / 2],
        times[SAMPLES * 95 / 100 - 1]
    );
}
fn compare(
    case: &str,
    n: usize,
    old_iters: usize,
    new_iters: usize,
    mut old: impl FnMut(),
    mut new: impl FnMut(),
) {
    let mut before = Vec::new();
    let mut after = Vec::new();
    for sample in 0..SAMPLES + 4 {
        let (a, b) = if sample % 2 == 0 {
            (batch(old_iters, &mut old), batch(new_iters, &mut new))
        } else {
            let b = batch(new_iters, &mut new);
            (batch(old_iters, &mut old), b)
        };
        if sample >= 4 {
            before.push(a);
            after.push(b);
        }
    }
    emit(&format!("{case}_legacy"), n, old_iters, &mut before);
    emit(&format!("{case}_exact_only"), n, new_iters, &mut after);
}
fn report(case: &str, n: usize, iterations: usize, mut prepare: impl FnMut(), mut f: impl FnMut()) {
    let mut times = Vec::new();
    for sample in 0..SAMPLES + 4 {
        prepare();
        let time = batch(iterations, &mut f);
        if sample >= 4 {
            times.push(time);
        }
    }
    emit(case, n, iterations, &mut times);
}
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--path-probe") {
        path_probe(std::env::args().nth(2).unwrap().parse().unwrap());
        return;
    }
    println!("case,fixture_size,iterations_per_sample,samples,median_ns_per_op,p95_ns_per_op");
    if std::env::args().nth(1).as_deref() == Some("--cached-path-only") {
        let root = std::env::temp_dir().join(format!("dogesh-cache-bench-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let scratch = Scratch(root);
        cached_path_cases(&scratch.0);
        return;
    }
    for n in [1, 500, 1_000, 10_000] {
        let cache = CompletionCache::new(Duration::from_secs(3600));
        cache.set_scoped(
            7,
            "git ".into(),
            (0..n)
                .map(|i| EnhancedCandidate {
                    text: format!("feature/{i:06}"),
                    description: Some("benchmark candidate".into()),
                    candidate_type: CandidateType::Generic,
                    priority: 0,
                })
                .collect(),
        );
        let iters = (20_000 / n).max(20);
        compare(
            "exact_hit",
            n,
            iters,
            iters,
            || {
                black_box(
                    cache
                        .lookup_scoped(7, black_box("git "))
                        .filter(|h| h.exact),
                );
            },
            || {
                black_box(cache.lookup_scoped_exact(7, black_box("git ")));
            },
        );
        compare(
            "prefix_only_miss",
            n,
            iters,
            20_000,
            || {
                black_box(
                    cache
                        .lookup_scoped(7, black_box("git f"))
                        .filter(|h| h.exact),
                );
            },
            || {
                black_box(cache.lookup_scoped_exact(7, black_box("git f")));
            },
        );
    }
    let root = std::env::temp_dir().join(format!("dogesh-cache-bench-{}", std::process::id()));
    fs::create_dir(&root).expect("unique scratch directory");
    let scratch = Scratch(root);
    cached_path_cases(&scratch.0);
    let mut paths = Vec::new();
    for i in 0..128 {
        let dir = scratch.0.join(format!("bin-{i}"));
        fs::create_dir(&dir).unwrap();
        for j in 0..64 {
            let file = dir.join(format!("fixture-command-{j:02}"));
            fs::write(&file, "").unwrap();
            fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
        }
        paths.push(dir.display().to_string());
    }
    for n in [1, 32, 128] {
        // Inherit PATH before Environment::new. Changing it through the shell
        // setter would start asynchronous prewarming and invalidate the empty
        // projection measurement halfway through its samples.
        assert!(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--path-probe", &n.to_string()])
                .env("PATH", paths[..n].join(":"))
                .status()
                .unwrap()
                .success()
        );
    }
}

fn cached_path_cases(root: &std::path::Path) {
    for n in [1, 100, 1_000, 10_000] {
        let dir = root.join(format!("files-{n}"));
        fs::create_dir(&dir).unwrap();
        for i in 0..n {
            fs::write(dir.join(format!("file{i:06}")), "").unwrap();
        }
        for (case, name, expected) in [
            ("cached_path_first", "file000000".to_string(), true),
            ("cached_path_last", format!("file{:06}", n - 1), true),
            ("cached_path_missing", "missing".to_string(), false),
        ] {
            let path = dir.join(name);
            report(
                case,
                n,
                1_000,
                || {
                    black_box(path_completion_path_sync(dir.clone()).unwrap());
                },
                || {
                    assert_eq!(black_box(is_path_cached(black_box(&path))), expected);
                },
            );
        }
    }
}

fn path_probe(n: usize) {
    let env = Environment::new();
    report(
        "path_empty_projection_miss",
        n,
        20,
        || {},
        || {
            assert!(
                black_box(env.read().search_prefix(black_box("zz-no-fixture-command"))).is_none()
            );
        },
    );
    let paths: Vec<_> = std::env::var("PATH")
        .unwrap()
        .split(':')
        .map(str::to_owned)
        .collect();
    env.write()
        .set_executable_names(collect_executables(&paths));
    report(
        "path_ready_projection_miss",
        n,
        20_000,
        || {},
        || {
            assert!(
                black_box(env.read().search_prefix(black_box("zz-no-fixture-command"))).is_none()
            );
        },
    );
}
