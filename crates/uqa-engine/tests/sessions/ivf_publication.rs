//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exercise the native session fixtures in one process even when the outer runner isolates tests.

#[test]
fn native_session_groups_preserve_metadata_under_concurrent_load() {
    let executable = std::env::current_exe().unwrap();
    // Fixed rounds, not retries: both schedules must pass every time. The parallel group also supplies real storage load while the serial group runs.
    for round in 0..2 {
        std::thread::scope(|scope| {
            let runs = [1, 16].map(|threads| {
                let executable = &executable;
                scope.spawn(move || {
                    let output = std::process::Command::new(executable)
                        .args([
                            "storage::sessions::native_",
                            "--test-threads",
                            &threads.to_string(),
                        ])
                        .output()
                        .unwrap();
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    assert!(
                        output.status.success()
                            && stdout.contains("69 passed; 0 failed; 0 ignored"),
                        "native session round {round}, threads {threads}: {}\n{stdout}\n{}",
                        output.status,
                        String::from_utf8_lossy(&output.stderr)
                    );
                })
            });
            for run in runs {
                run.join().unwrap();
            }
        });
    }
}
