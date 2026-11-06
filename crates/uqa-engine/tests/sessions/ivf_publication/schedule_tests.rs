//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{run, Progress};
use std::process::Command;
use std::time::{Duration, Instant};

const PREFIX: &str = "schedule_probe::";

#[test]
fn fixture_progress_owns_the_deadline_without_limiting_the_complete_history() {
    let start = Instant::now();
    let allowance = Duration::from_secs(300);
    let mut progress = Progress::new(PREFIX, 2, allowance, start);
    progress
        .observe(
            "test schedule_probe::one ... ok",
            start + Duration::from_secs(290),
        )
        .unwrap();
    progress
        .observe("diagnostic output", start + Duration::from_secs(580))
        .unwrap();
    assert_eq!(
        progress
            .remaining(start + Duration::from_secs(585))
            .unwrap(),
        Duration::from_secs(5)
    );
    progress
        .observe(
            "test schedule_probe::two ... ok",
            start + Duration::from_secs(589),
        )
        .unwrap();
    progress
        .observe(
            "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;",
            start + Duration::from_secs(590),
        )
        .unwrap();
    assert!(progress.remaining(start + Duration::from_secs(888)).is_ok());
    assert!(progress
        .remaining(start + Duration::from_secs(889))
        .is_err());
    progress.finish().unwrap();
}

#[test]
fn repeated_or_unrelated_results_cannot_extend_fixture_progress() {
    let start = Instant::now();
    for line in [
        "test schedule_probe::one ... ok",
        "test unrelated::two ... ok",
        "test schedule_probe::two ... FAILED",
        "test result: ok. 2 passed; 0 failed; 0 ignored;",
    ] {
        let mut progress = Progress::new(PREFIX, 2, Duration::from_secs(300), start);
        progress
            .observe("test schedule_probe::one ... ok", start)
            .unwrap();
        assert!(progress
            .observe(line, start + Duration::from_secs(299))
            .is_err());
        assert!(progress
            .remaining(start + Duration::from_secs(300))
            .is_err());
    }
}

fn probe(mode: &str, allowance: Duration) -> Result<(), String> {
    let module = module_path!().split_once("::").unwrap().1;
    run(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("{module}::child_fixture"),
                "--ignored",
                "--nocapture",
                "--format=terse",
            ])
            .env("UQA_NATIVE_SCHEDULE_PROBE", mode),
        PREFIX,
        2,
        allowance,
        mode,
    )
}

#[test]
fn completed_child_requires_every_fixture_and_successful_process_exit() {
    probe("complete", Duration::from_secs(30)).unwrap();
    for (mode, message) in [
        ("missing", "incomplete child suite"),
        ("duplicate", "duplicate or extra fixture"),
        ("failed_case", "unexpected fixture result"),
        ("failed_exit", "child suite failed"),
    ] {
        let error = probe(mode, Duration::from_secs(30)).unwrap_err();
        assert!(error.contains(message), "{mode}: {error}");
    }
}

#[test]
fn stalled_or_noisy_children_are_terminated_and_reaped() {
    for mode in ["stalled", "noisy"] {
        let error = probe(mode, Duration::from_secs(2)).unwrap_err();
        assert!(
            error.contains("no new fixture completed"),
            "{mode}: {error}"
        );
    }
}

#[test]
#[ignore = "subprocess entry point exercised by the schedule lifecycle tests"]
fn child_fixture() {
    let mode = std::env::var("UQA_NATIVE_SCHEDULE_PROBE").unwrap();
    println!("test schedule_probe::one ... ok");
    match mode.as_str() {
        "missing" => std::process::exit(0),
        "duplicate" => println!("test schedule_probe::one ... ok"),
        "failed_case" => println!("test schedule_probe::two ... FAILED"),
        "stalled" => loop {
            std::thread::park();
        },
        "noisy" => loop {
            println!("diagnostic output cannot extend the deadline");
            std::thread::sleep(Duration::from_millis(10));
        },
        "complete" | "failed_exit" => {
            println!("test schedule_probe::two ... ok");
            println!("test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;");
        }
        _ => panic!("unknown schedule probe"),
    }
    std::process::exit(if mode == "failed_exit" { 23 } else { 0 });
}
