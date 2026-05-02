use std::env;

fn main() {
    let iterations = env::var("DSH_PERF_ITERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1_000);

    println!("doge-shell latency probes: iterations={iterations}");
    for result in doge_shell::perf_probes::run_default_probes(iterations) {
        let total_us = result.elapsed.as_micros();
        let avg_ns = result.elapsed.as_nanos() / result.iterations.max(1) as u128;
        println!(
            "{:<32} total={:>8}us avg={:>8}ns iterations={}",
            result.name, total_us, avg_ns, result.iterations
        );
    }
}
