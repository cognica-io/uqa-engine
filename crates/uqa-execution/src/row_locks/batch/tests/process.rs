//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Concurrent processes retain independent keys with bounded claim arbitration.

use super::*;
use std::io::{BufRead, Write};
use std::process::{Child, Stdio};
use std::sync::{atomic::Ordering, mpsc};
use std::time::Duration;

const PREFIX: &str = "UQA_PARALLEL_KEYS ";
const PEER: &str = "row_locks::batch::tests::process::parallel_key_peer";

fn digest(process: u64, key: u64) -> [u8; 32] {
    let mut value = [0; 32];
    value[..8].copy_from_slice(&process.to_be_bytes());
    value[24..].copy_from_slice(&key.to_be_bytes());
    value
}

fn request(table: u64, cancel: &CancellationToken) -> LockRequest<'_> {
    LockRequest {
        session_id: 1,
        key: RowLockKey { table, doc_id: 0 },
        strength: LockStrength::ForUpdate,
        mark: 1,
        wait: LockWait::NoWait,
        cancel,
        relation: "parallel keys",
    }
}

struct Peer {
    child: Child,
    responses: mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Peer {
    fn new(path: &std::path::Path, process: u64, count: u64) -> Self {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                PEER,
                "--nocapture",
                "--test-threads=1",
            ])
            .env("UQA_PARALLEL_KEYS_PATH", path)
            .env("UQA_PARALLEL_KEYS_PROCESS", process.to_string())
            .env("UQA_PARALLEL_KEYS_COUNT", count.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, responses) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some((_, response)) = line.split_once(PREFIX) {
                    if sender.send(response.to_owned()).is_err() {
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

    fn send(&mut self, command: &str) {
        let input = self.child.stdin.as_mut().unwrap();
        writeln!(input, "{command}").unwrap();
        input.flush().unwrap();
    }

    fn response(&mut self) -> String {
        self.responses
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|error| {
                panic!(
                    "key peer failed: {error}; status {:?}",
                    self.child.try_wait()
                )
            })
    }

    fn finish(&mut self) {
        assert!(self.child.wait().unwrap().success());
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
    println!("{PREFIX}{value}");
    std::io::stdout().flush().unwrap();
}

#[test]
#[ignore = "subprocess entry point for parallel key reservations"]
fn parallel_key_peer() {
    let path = std::env::var_os("UQA_PARALLEL_KEYS_PATH").unwrap();
    let process = std::env::var("UQA_PARALLEL_KEYS_PROCESS")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let count = std::env::var("UQA_PARALLEL_KEYS_COUNT")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let manager = RowLockManager::for_database_file(std::path::Path::new(&path));
    let coordinator = manager.coordinator().unwrap().unwrap();
    let cancel = CancellationToken::new();
    let mut commands = std::io::stdin().lock().lines();
    respond("ready");
    assert_eq!(commands.next().unwrap().unwrap(), "acquire");
    let identities = (0..count)
        .map(|key| manager.retain_key_reservation(digest(process, key)))
        .collect::<Vec<_>>();
    let wanted = identities
        .iter()
        .map(|identity| request(identity.table_key(), &cancel))
        .collect::<Vec<_>>();
    let before = coordinator.row_claim_table_locks.load(Ordering::Relaxed);
    let granted = manager.acquire_batch(&wanted).unwrap();
    let arbitrations = coordinator.row_claim_table_locks.load(Ordering::Relaxed) - before;
    assert_eq!(granted.len(), count as usize);
    assert!(granted.iter().all(|grant| matches!(
        grant,
        LockAcquire::Granted {
            acquisition: Some(_),
            waited: false,
            foreign_waited: false,
        }
    )));
    assert_eq!(arbitrations, count.div_ceil(64) as usize);
    assert_eq!(coordinator.relation_registry_counts(), (0, 0));
    assert_eq!(manager.state.lock().rows.len(), count as usize);
    drop(identities);
    assert_eq!(manager.identities.retained_counts().0, count as usize);
    respond(format!("held {count} {arbitrations}"));
    assert_eq!(commands.next().unwrap().unwrap(), "release");
    manager.release_session(1);
    assert!(manager.state.lock().rows.is_empty());
    assert_eq!(manager.identities.retained_counts().0, 0);
    assert_eq!(coordinator.relation_registry_counts(), (0, 0));
    respond("released");
}

fn probe(manager: &RowLockManager, process: u64, count: u64, held: bool) {
    let cancel = CancellationToken::new();
    for key in [0, count - 1] {
        let identity = manager.retain_key_reservation(digest(process, key));
        let result = manager.acquire(&request(identity.table_key(), &cancel));
        if held {
            assert_eq!(result.unwrap_err().sqlstate(), Some("55P03"));
        } else {
            assert!(matches!(result.unwrap(), LockAcquire::Granted { .. }));
            manager.release_session(1);
        }
    }
    assert_eq!(manager.identities.retained_counts().0, 0);
}

#[test]
fn parallel_processes_bound_key_arbitration_and_preserve_independent_release() {
    for processes in [2, 4] {
        for count in [64, 512] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("parallel.db");
            let manager = RowLockManager::for_database_file(&path);
            let mut peers = (0..processes)
                .map(|process| Peer::new(&path, process, count))
                .collect::<Vec<_>>();
            // All peers are ready before any starts; no response serializes their acquisitions.
            for peer in &mut peers {
                peer.send("acquire");
            }
            for peer in &mut peers {
                assert_eq!(
                    peer.response(),
                    format!("held {count} {}", count.div_ceil(64))
                );
            }
            for process in 0..processes {
                probe(&manager, process, count, true);
            }
            for (released, peer) in peers.iter_mut().enumerate() {
                peer.send("release");
                assert_eq!(peer.response(), "released");
                peer.finish();
                for process in 0..processes {
                    probe(&manager, process, count, process > released as u64);
                }
            }
            assert!(manager.state.lock().rows.is_empty());
        }
    }
}
