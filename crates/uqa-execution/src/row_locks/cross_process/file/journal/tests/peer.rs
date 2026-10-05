//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reader watermarks and reclamation with independent operating-system processes.

use std::io::{BufRead, Write};
use std::process::{Child, Stdio};

use super::*;

const PREFIX: &str = "UQA_JOURNAL_RESPONSE ";

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
                "row_locks::cross_process::file::journal::tests::peer::journal_peer",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("UQA_JOURNAL_TEST_PATH", path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, responses) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some((_, reply)) = line.split_once(PREFIX) {
                    if sender.send(reply.to_owned()).is_err() {
                        break;
                    }
                }
            }
        });
        let peer = Self {
            child,
            responses,
            reader: Some(reader),
        };
        assert_eq!(peer.response(), "ready");
        peer
    }

    fn response(&self) -> String {
        self.responses
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("journal peer response")
    }

    fn request(&mut self, command: &str) -> String {
        let input = self.child.stdin.as_mut().unwrap();
        writeln!(input, "{command}").unwrap();
        input.flush().unwrap();
        self.response()
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
            reader.join().unwrap();
        }
    }
}

#[test]
#[ignore = "subprocess entry point for journal lease tests"]
fn journal_peer() {
    let path = std::env::var_os("UQA_JOURNAL_TEST_PATH").expect("journal test path");
    let coordinator = Arc::new(FileLockCoordinator::open(std::path::Path::new(&path)).unwrap());
    let mut retained = None;
    println!("{PREFIX}ready");
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let reply = match line.as_str() {
            "pin" => {
                retained = Some(coordinator.pin_change_sequence().unwrap());
                retained.as_ref().unwrap().0.to_string()
            }
            "drop" => {
                retained = None;
                "dropped".into()
            }
            "append" => {
                coordinator.publish_changes(&[update(1)]).unwrap();
                "appended".into()
            }
            "lookup" => format!(
                "{:?}",
                coordinator
                    .change_target_after(
                        TABLE,
                        1,
                        retained.as_ref().unwrap().0,
                        LockStrength::ForUpdate
                    )
                    .unwrap()
            ),
            _ => panic!("unexpected command {line}"),
        };
        println!("{PREFIX}{reply}");
        std::io::stdout().flush().unwrap();
    }
}

#[test]
fn live_peers_pin_history_and_killed_peers_stop_retaining_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("peer.db");
    let coordinator = Arc::new(FileLockCoordinator::open(&path).unwrap());
    let mut peer = Peer::start(&path);
    assert_eq!(peer.request("pin"), "0");
    for _ in 0..10 {
        let (_, lease) = coordinator.pin_change_sequence().unwrap();
        coordinator.publish_changes(&[update(1)]).unwrap();
        drop(lease);
    }
    assert_eq!(peer.request("lookup"), "Present(1)");
    assert_eq!(
        std::fs::metadata(&coordinator.change_path).unwrap().len(),
        10 * CHANGE_ENTRY_SIZE
    );
    peer.terminate();
    let (sequence, lease) = coordinator.pin_change_sequence().unwrap();
    assert_eq!(sequence, 10);
    coordinator.publish_changes(&[update(2)]).unwrap();
    assert_eq!(
        std::fs::metadata(&coordinator.change_path).unwrap().len(),
        HEADER_BYTES + CHANGE_ENTRY_SIZE
    );
    drop(lease);
    assert_eq!(
        std::fs::metadata(&coordinator.change_path).unwrap().len(),
        HEADER_BYTES
    );
}

#[test]
fn a_peer_publishes_after_another_process_replaces_the_generation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("replace.db");
    let coordinator = Arc::new(FileLockCoordinator::open(&path).unwrap());
    let mut peer = Peer::start(&path);
    for sequence in 0..10 {
        let (baseline, lease) = coordinator.pin_change_sequence().unwrap();
        assert_eq!(baseline, sequence);
        assert_eq!(peer.request("append"), "appended");
        assert_eq!(
            coordinator
                .change_target_after(TABLE, 1, baseline, LockStrength::ForUpdate)
                .unwrap(),
            RowChangeTarget::Present(1)
        );
        drop(lease);
        assert_eq!(
            std::fs::metadata(&coordinator.change_path).unwrap().len(),
            HEADER_BYTES
        );
    }
}
