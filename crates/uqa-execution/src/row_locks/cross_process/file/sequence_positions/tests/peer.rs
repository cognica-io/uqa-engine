//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A second process that advances sequences of the same database.

use std::io::{BufRead, Write};
use std::process::{Child, Stdio};

use super::*;

const RESPONSE_PREFIX: &str = "UQA_SEQUENCE_POSITION_RESPONSE ";

struct Peer {
    child: Child,
    responses: std::sync::mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Peer {
    fn start(path: &std::path::Path) -> Self {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "row_locks::cross_process::file::sequence_positions::tests::peer::sequence_position_peer",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("UQA_SEQUENCE_POSITION_TEST_PATH", path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, responses) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some((_, response)) = line.split_once(RESPONSE_PREFIX) {
                    if send.send(response.to_owned()).is_err() {
                        break;
                    }
                }
            }
        });
        let mut peer = Self {
            child,
            responses,
            reader: Some(reader),
        };
        assert_eq!(peer.response(), "ready");
        peer
    }

    fn request(&mut self, request: &str) -> String {
        let input = self.child.stdin.as_mut().unwrap();
        writeln!(input, "{request}").unwrap();
        input.flush().unwrap();
        self.response()
    }

    fn response(&mut self) -> String {
        self.responses
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap_or_else(|error| {
                panic!(
                    "sequence position peer response failed: {error}; status {:?}",
                    self.child.try_wait()
                )
            })
    }

    /// End the peer the way an orderly close does.
    fn close(mut self) {
        assert_eq!(self.request("close"), "closed");
        self.child.wait().unwrap();
    }

    fn terminate(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn respond(value: impl std::fmt::Display) {
    println!("{RESPONSE_PREFIX}{value}");
    std::io::stdout().flush().unwrap();
}

/// `record SEQUENCE CURRENT`, `read SEQUENCE`, `advance SEQUENCE COUNT` and `close`.
#[test]
#[ignore = "subprocess entry point for the sequence position tests"]
fn sequence_position_peer() {
    let path = std::env::var_os("UQA_SEQUENCE_POSITION_TEST_PATH").unwrap();
    let coordinator = FileLockCoordinator::open(std::path::Path::new(&path)).unwrap();
    respond("ready");
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let mut command = line.split_whitespace();
        let operation = command.next().unwrap();
        let mut number = || command.next().map(|word| word.parse::<i64>().unwrap());
        match operation {
            "record" => {
                let sequence = number().unwrap() as u32;
                let recorded = Locked::new(&coordinator).record(sequence, number().unwrap());
                respond(if recorded { "recorded" } else { "full" });
            }
            "read" => match Locked::new(&coordinator).recorded(number().unwrap() as u32) {
                Some((current, fresh)) => respond(format!("{current} {fresh}")),
                None => respond("none"),
            },
            "advance" => {
                let sequence = number().unwrap() as u32;
                for _ in 0..number().unwrap() {
                    let positions = Locked::new(&coordinator);
                    let current = positions
                        .recorded(sequence)
                        .map_or(0, |(current, _)| current);
                    assert!(positions.record(sequence, current + 1));
                }
                respond("advanced");
            }
            "close" => break,
            _ => panic!("unexpected sequence position peer command {line}"),
        }
    }
    drop(coordinator);
    respond("closed");
}

#[test]
fn processes_read_and_advance_the_same_positions() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shared.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    assert!(Locked::new(&coordinator).record(1, 10));
    let mut peer = Peer::start(&path);
    // The peer attached to a running database, so it reads the position as one of this run.
    assert_eq!(peer.request("read 1"), "10 true");
    assert_eq!(peer.request("record 1 11"), "recorded");
    assert_eq!(peer.request("record 2 70"), "recorded");
    let positions = Locked::new(&coordinator);
    assert_eq!(positions.recorded(1), Some((11, true)));
    assert_eq!(positions.recorded(2), Some((70, true)));
    positions.remove(2);
    drop(positions);
    assert_eq!(peer.request("read 2"), "none");
}

#[test]
fn concurrent_processes_never_lose_an_advance() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("advance.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let mut peer = Peer::start(&path);
    let input = peer.child.stdin.as_mut().unwrap();
    writeln!(input, "advance 5 2000").unwrap();
    input.flush().unwrap();
    for _ in 0..2000 {
        let positions = Locked::new(&coordinator);
        let current = positions.recorded(5).map_or(0, |(current, _)| current);
        assert!(positions.record(5, current + 1));
    }
    assert_eq!(peer.response(), "advanced");
    assert_eq!(Locked::new(&coordinator).recorded(5), Some((4000, true)));
}

#[test]
fn the_positions_of_a_killed_process_stay_while_another_process_is_attached() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("survivor.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    assert!(Locked::new(&coordinator).record(9, 1));
    let mut peer = Peer::start(&path);
    assert_eq!(peer.request("record 1 10"), "recorded");
    peer.terminate();
    // The file both processes read holds what the killed one wrote.
    assert_eq!(Locked::new(&coordinator).recorded(1), Some((10, true)));
}

#[test]
fn the_positions_of_a_killed_process_are_discarded_when_no_process_remains() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("killed.db");
    let mut peer = Peer::start(&path);
    assert_eq!(peer.request("record 1 10"), "recorded");
    peer.terminate();
    // Nothing tells a killed process from a failed machine, whose file may be older than the values handed out.
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    assert_eq!(Locked::new(&coordinator).recorded(1), None);
}

#[test]
fn the_positions_of_a_closed_process_are_kept_for_the_next_run() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("closed.db");
    let mut peer = Peer::start(&path);
    assert_eq!(peer.request("record 1 10"), "recorded");
    peer.close();
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    assert_eq!(Locked::new(&coordinator).recorded(1), Some((10, false)));
}

#[test]
fn the_last_process_to_close_marks_the_positions_of_every_process_clean() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("last.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    assert!(Locked::new(&coordinator).record(1, 10));
    let mut peer = Peer::start(&path);
    assert_eq!(peer.request("record 2 20"), "recorded");
    // The process that closes first leaves the mark to the one still attached.
    drop(coordinator);
    assert_eq!(peer.request("read 1"), "10 true");
    peer.close();
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    assert!(coordinator.stored_sequence_header().unwrap().unwrap().clean);
    let positions = Locked::new(&coordinator);
    assert_eq!(positions.recorded(1), Some((10, false)));
    assert_eq!(positions.recorded(2), Some((20, false)));
}
