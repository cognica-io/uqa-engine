//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{run, OwnedChild, Progress};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

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

fn test_command(name: &str) -> Command {
    let module = module_path!().split_once("::").unwrap().1;
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", &format!("{module}::{name}")]);
    command
}

fn child_command(mode: &str) -> Command {
    let mut command = test_command("child_fixture");
    command
        .args(["--ignored", "--nocapture", "--format=terse"])
        .env("UQA_NATIVE_SCHEDULE_PROBE", mode);
    command
}

fn probe(mode: &str, allowance: Duration) -> Result<(), String> {
    run(&mut child_command(mode), PREFIX, 2, allowance, mode)
}

#[test]
fn observed_child_exit_precedes_an_expired_progress_deadline() {
    for (mode, success) in [("complete", true), ("failed_exit", false)] {
        let capture = NamedTempFile::new().unwrap();
        let writer = capture.as_file().try_clone().unwrap();
        let mut child = OwnedChild::spawn(&mut child_command(mode), capture).unwrap();
        assert_eq!(
            child.process.as_mut().unwrap().wait().unwrap().success(),
            success
        );
        let progress = Progress::new(PREFIX, 2, Duration::ZERO, Instant::now());
        let result = child.watch(progress, mode);
        if success {
            result.unwrap();
        } else {
            assert!(result.unwrap_err().contains("23"));
        }
        assert!(child.process.is_none());
        assert!(writer.metadata().unwrap().len() > 0);
    }
}

#[test]
fn buffered_results_and_closed_output_precede_an_expired_observation_deadline() {
    let module = module_path!().split_once("::").unwrap().1;
    let mut command = test_command("repeated_or_unrelated_results_cannot_extend_fixture_progress");
    command.arg("--format=pretty");
    let capture = NamedTempFile::new().unwrap();
    let mut trailing_output = capture.as_file().try_clone().unwrap();
    let mut child = OwnedChild::spawn(&mut command, capture).unwrap();
    assert!(child.process.as_mut().unwrap().wait().unwrap().success());
    writeln!(trailing_output).unwrap();
    drop(trailing_output);
    child
        .watch(
            Progress::new(module, 1, Duration::ZERO, Instant::now()),
            "libtest",
        )
        .unwrap();
}

#[test]
fn buffered_diagnostics_do_not_extend_a_running_childs_deadline() {
    let mut capture = NamedTempFile::new().unwrap();
    capture.write_all(&vec![b' '; 64 * 1024]).unwrap();
    writeln!(
        capture,
        "\ntest schedule_probe::one ... ok\ntest schedule_probe::two ... ok"
    )
    .unwrap();
    writeln!(capture, "test result: ok. 2 passed; 0 failed; 0 ignored;").unwrap();
    let mut child = OwnedChild::spawn(&mut child_command("silent_stall"), capture).unwrap();
    let progress = Progress::new(PREFIX, 2, Duration::ZERO, Instant::now());
    let error = child.watch(progress, "backlog").unwrap_err();
    assert!(
        error.contains("completed 2/2; successful summary observed, child still running"),
        "{error}"
    );
}

#[test]
fn completed_child_requires_every_fixture_and_successful_process_exit() {
    probe("complete", Duration::from_secs(30)).unwrap();
    for (mode, message) in [
        ("missing", "incomplete child suite"),
        ("duplicate", "duplicate or extra fixture"),
        ("failed_case", "unexpected fixture result"),
        ("failed_exit", "child suite failed"),
        ("late_duplicate", "duplicate or extra fixture"),
        ("late_summary", "unexpected fixture summary"),
    ] {
        let error = probe(mode, Duration::from_secs(30)).unwrap_err();
        assert!(error.contains(message), "{mode}: {error}");
    }
}

fn wait_for_marker(path: &Path) {
    let stop = Instant::now() + Duration::from_secs(60);
    while !path.exists() {
        assert!(Instant::now() < stop, "missing marker {}", path.display());
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn independent_stdout_writer_does_not_own_a_direct_childs_exit() {
    for (mode, success) in [("complete", true), ("failed_exit", false)] {
        let directory = tempfile::tempdir().unwrap();
        let markers = directory.path();
        let result = std::thread::scope(|scope| {
            let (input, release_writer) = std::io::pipe().unwrap();
            let (send, receive) = std::sync::mpsc::channel();
            scope.spawn(move || {
                let mut command = child_command(mode);
                command
                    .stdin(input)
                    .env("UQA_NATIVE_SCHEDULE_WRITER", markers);
                send.send(run(&mut command, PREFIX, 2, Duration::from_secs(60), mode))
                    .unwrap();
            });
            let outcome = receive.recv_timeout(Duration::from_secs(90));
            drop(release_writer);
            wait_for_marker(&markers.join("released"));
            outcome.unwrap()
        });
        if success {
            result.unwrap();
        } else {
            let error = result.unwrap_err();
            assert!(
                error.contains("child suite failed") && error.contains("23"),
                "{error}"
            );
        }
    }
}

#[test]
fn stalled_or_noisy_children_are_terminated_and_reaped() {
    for mode in ["stalled", "noisy", "complete_stalled"] {
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
    if let Some(markers) = std::env::var_os("UQA_NATIVE_SCHEDULE_WRITER") {
        let markers = Path::new(&markers);
        if mode == "hold_stdout" {
            std::fs::write(markers.join("holding"), []).unwrap();
            assert_eq!(std::io::stdin().read(&mut [0]).unwrap(), 0);
            std::fs::write(markers.join("released"), []).unwrap();
            std::process::exit(0);
        }
        // The observer owns this writer's release through the inherited stdin pipe.
        let holder = child_command("hold_stdout")
            .stdin(Stdio::inherit())
            .spawn()
            .unwrap();
        wait_for_marker(&markers.join("holding"));
        drop(holder);
    }
    if mode == "silent_stall" {
        loop {
            std::thread::park();
        }
    }
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
        "complete" | "failed_exit" | "complete_stalled" | "late_duplicate" | "late_summary" => {
            println!("test schedule_probe::two ... ok");
            println!("test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;");
            match mode.as_str() {
                "complete_stalled" => loop {
                    std::thread::park();
                },
                "late_duplicate" => print!("test schedule_probe::two ... ok"),
                "late_summary" => print!("test result: ok. 2 passed; 0 failed; 0 ignored;"),
                _ => println!(),
            }
        }
        _ => panic!("unknown schedule probe"),
    }
    std::io::stdout().flush().unwrap();
    std::process::exit(if mode == "failed_exit" { 23 } else { 0 });
}
