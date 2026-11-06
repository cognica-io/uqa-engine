//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exercise the native session fixtures in one process even when the outer runner isolates tests.

#[path = "ivf_publication/schedule.rs"]
mod schedule;

#[rstest::rstest]
#[case::first(0)]
#[case::second(1)]
fn native_session_groups_preserve_metadata_under_concurrent_load(#[case] round: usize) {
    use std::process::Command;
    use std::time::Duration;

    let executable = std::env::current_exe().unwrap();
    // Both fixed rounds are required cases. Each parallel group supplies storage load while its serial group runs.
    std::thread::scope(|scope| {
        let runs = [1, 16].map(|threads| {
            let executable = &executable;
            scope.spawn(move || {
                eprintln!("native session round {round}, threads {threads}: starting");
                schedule::run(
                    Command::new(executable).args([
                        "storage::sessions::native_",
                        "--test-threads",
                        &threads.to_string(),
                    ]),
                    "storage::sessions::native_",
                    69,
                    Duration::from_secs(300),
                    &format!("native session round {round}, threads {threads}"),
                )
                .unwrap();
            })
        });
        for run in runs {
            run.join().unwrap();
        }
    });
}
