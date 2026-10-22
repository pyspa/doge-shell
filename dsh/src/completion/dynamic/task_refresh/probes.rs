//! Explicitly run controlled CLI probe; these timings are not a real Nx project benchmark.
use super::*;
use crate::completion::dynamic::runtime::CompletionWorkerCounts;
use crate::environment::Environment;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
use std::sync::{Arc, mpsc};

fn fifo(path: &Path) -> fs::File {
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}

#[test]
#[ignore = "explicit controlled CLI latency measurement"]
fn controlled_nx_cli_latency_probe() {
    const SAMPLES: usize = 20;
    for asynchronous in [false, true] {
        for scenario in ["cold", "warm", "invalidated", "expired", "queue-contention"] {
            let mut returns = Vec::new();
            let mut ready = Vec::new();
            let dir = tempfile::Builder::new()
                .prefix("dogesh-nx-probe-")
                .tempdir_in("/tmp")
                .unwrap();
            fs::write(dir.path().join("nx.json"), "{}").unwrap();
            let bin = dir.path().join("node_modules/.bin");
            fs::create_dir_all(&bin).unwrap();
            let ready_path = dir.path().join("ready");
            let release_path = dir.path().join("release");
            let ready_pipe = fifo(&ready_path);
            let release_pipe = fifo(&release_path);
            let nx = bin.join("nx");
            fs::write(&nx, format!("#!/bin/sh\nif [ \"$2\" = projects ]; then\nprintf x > '{}'\nIFS= read -r _ < '{}'\nprintf '[\"api\"]'\nelse\nprintf '{{\"targets\":{{\"build\":{{}}}}}}'\nfi\n", ready_path.display(), release_path.display())).unwrap();
            fs::set_permissions(&nx, fs::Permissions::from_mode(0o755)).unwrap();
            let env = Environment::new_synthetic();
            env.write().variable_state.paths.clear();
            let seed_provider = DynamicCompletionProvider::new(env.clone());
            let seed_signature =
                TaskDiscoveryRequest::new(&seed_provider, dir.path(), &["nx"]).signature;
            if scenario == "expired" {
                let since_epoch = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap();
                let until_next_epoch =
                    Duration::from_millis(5010 - (since_epoch.as_millis() % 5000) as u64);
                let (_tx, rx) = mpsc::channel::<()>();
                let _ = rx.recv_timeout(until_next_epoch);
            }
            for sample in 0..SAMPLES {
                let mut ready_pipe = ready_pipe.try_clone().unwrap();
                let mut release_pipe = release_pipe.try_clone().unwrap();
                let runtime = Arc::new(super::super::CompletionRuntime::with_worker_counts(
                    CompletionWorkerCounts::new(1, 1, 1),
                ));
                let provider =
                    DynamicCompletionProvider::with_runtime(env.clone(), runtime.clone());
                if matches!(scenario, "warm" | "invalidated" | "expired") {
                    let request = TaskDiscoveryRequest::new(&provider, dir.path(), &["nx"]);
                    provider.cache.write().tasks.insert(
                        request.cache_key,
                        TaskCacheEntry {
                            signature: if scenario == "expired" {
                                seed_signature
                            } else {
                                request.signature
                            },
                            tasks: vec![task::TaskInfo::new(
                                "nx",
                                "api:build",
                                "nx run api:build",
                                dir.path().to_string_lossy().into_owned(),
                            )],
                        },
                    );
                    if scenario == "invalidated" {
                        fs::write(
                            dir.path().join("nx.json"),
                            format!("{{\"sample\":{sample}}}"),
                        )
                        .unwrap();
                    }
                }
                let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel();
                runtime.set_notifier(notify_tx);
                let controller = (scenario != "warm").then(|| {
                    std::thread::spawn(move || {
                        let mut byte = [0];
                        let mut poll = libc::pollfd {
                            fd: ready_pipe.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        };
                        assert_eq!(
                            unsafe { libc::poll(&mut poll, 1, 5000) },
                            1,
                            "controlled loader did not start"
                        );
                        ready_pipe.read_exact(&mut byte).unwrap();
                        // Deliberate provider hold, not a polling loop.
                        let (_tx, rx) = mpsc::channel::<()>();
                        assert!(matches!(
                            rx.recv_timeout(Duration::from_millis(25)),
                            Err(mpsc::RecvTimeoutError::Timeout)
                        ));
                        release_pipe.write_all(b"go\n").unwrap();
                    })
                });
                let (unblock_tx, unblock_rx) = mpsc::channel::<()>();
                let blocker = if scenario == "queue-contention" {
                    let (started_tx, started_rx) = mpsc::channel();
                    assert!(runtime.submit_command(Box::new(move || {
                        started_tx.send(()).unwrap();
                        unblock_rx.recv().unwrap();
                    })));
                    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    Some(std::thread::spawn(move || {
                        let (_tx, rx) = mpsc::channel::<()>();
                        let _ = rx.recv_timeout(Duration::from_millis(25));
                        unblock_tx.send(()).unwrap();
                    }))
                } else {
                    None
                };
                let start = Instant::now();
                let result = if asynchronous {
                    provider
                        .load_or_schedule_project_tasks_for_sources(dir.path(), &["nx"])
                        .unwrap()
                } else {
                    Some(
                        provider
                            .load_project_tasks_for_sources(dir.path(), &["nx"])
                            .unwrap(),
                    )
                };
                returns.push(start.elapsed().as_secs_f64() * 1000.0);
                if result.is_none() {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    rt.block_on(async {
                        tokio::time::timeout(Duration::from_secs(5), notify_rx.recv())
                            .await
                            .unwrap()
                            .unwrap();
                    });
                }
                ready.push(start.elapsed().as_secs_f64() * 1000.0);
                assert!(
                    provider
                        .lookup_project_tasks_for_sources(dir.path(), &["nx"])
                        .iter()
                        .any(|t| t.name == "api:build")
                );
                if let Some(controller) = controller {
                    controller.join().unwrap();
                }
                if let Some(blocker) = blocker {
                    blocker.join().unwrap();
                }
            }
            returns.sort_by(f64::total_cmp);
            ready.sort_by(f64::total_cmp);
            println!(
                "nx-controlled mode={} scenario={scenario} n={SAMPLES} request_ms_p50={:.3} request_ms_p95={:.3} candidate_ready_ms_p50={:.3} candidate_ready_ms_p95={:.3} caller_blocked_ms_p50={:.3} caller_blocked_ms_p95={:.3}",
                if asynchronous {
                    "async"
                } else {
                    "baseline-sync"
                },
                returns[9],
                returns[18],
                ready[9],
                ready[18],
                returns[9],
                returns[18]
            );
        }
    }
}
