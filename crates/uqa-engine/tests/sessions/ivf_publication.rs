//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exercise the native session fixtures in one process even when the outer runner isolates tests.

#[rstest::rstest]
#[case::first(0)]
#[case::second(1)]
fn native_session_groups_preserve_metadata_under_concurrent_load(#[case] round: usize) {
    use std::io::BufRead;
    use std::process::{Command, Stdio};

    let executable = std::env::current_exe().unwrap();
    // Both fixed rounds are required cases. Each parallel group supplies storage load while its serial group runs.
    std::thread::scope(|scope| {
        let runs = [1, 16].map(|threads| {
            let executable = &executable;
            scope.spawn(move || {
                eprintln!("native session round {round}, threads {threads}: starting");
                let mut child = Command::new(executable)
                    .args([
                        "storage::sessions::native_",
                        "--test-threads",
                        &threads.to_string(),
                    ])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .unwrap();
                let mut complete = false;
                let progress = std::io::BufReader::new(child.stdout.take().unwrap())
                    .lines()
                    .try_for_each(|line| {
                        let line = line?;
                        complete |= line.contains("69 passed; 0 failed; 0 ignored");
                        eprintln!("native session round {round}, threads {threads}: {line}");
                        std::io::Result::Ok(())
                    });
                let status = child.wait().unwrap();
                progress.unwrap();
                assert!(
                    status.success() && complete,
                    "native session round {round}, threads {threads}: {status}; complete={complete}"
                );
            })
        });
        for run in runs {
            run.join().unwrap();
        }
    });
}
