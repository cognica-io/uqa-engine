//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A second process claiming rows of the same database.

use std::io::{BufRead, Write};
use std::process::{Child, Stdio};

use crate::row_locks::lock_strengths_conflict;

use super::*;

const RESPONSE_PREFIX: &str = "UQA_ROW_CLAIM_RESPONSE ";

struct Peer {
    child: Child,
    responses: std::sync::mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Peer {
    fn start(path: &std::path::Path) -> Self {
        Self::start_with_mapping(path, true)
    }

    fn start_with_mapping(path: &std::path::Path, mapping: bool) -> Self {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "row_locks::cross_process::file::row_claims::tests::peer::row_claim_peer",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("UQA_ROW_CLAIM_TEST_PATH", path)
            .env(
                "UQA_ROW_CLAIM_TEST_MAPPING",
                if mapping { "1" } else { "0" },
            )
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
                    "row claim peer response failed: {error}; status {:?}",
                    self.child.try_wait()
                )
            })
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

/// `claim` and `release` take a strength index, a first row, a row count and a session.
#[test]
#[ignore = "subprocess entry point for the row claim tests"]
fn row_claim_peer() {
    let path = std::env::var_os("UQA_ROW_CLAIM_TEST_PATH").unwrap();
    let coordinator = FileLockCoordinator::open(std::path::Path::new(&path)).unwrap();
    if std::env::var("UQA_ROW_CLAIM_TEST_MAPPING").as_deref() == Ok("0") {
        coordinator.claim_mapping.lock().disable();
    }
    respond("ready");
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let mut command = line.split_whitespace();
        let operation = command.next().unwrap();
        let mut number = || command.next().map(|word| word.parse::<u64>().unwrap());
        let strength = STRENGTHS[number().unwrap() as usize];
        let first = number().unwrap();
        let count = number().unwrap_or(1);
        let session = number().unwrap_or(PEER_SESSION);
        match operation {
            "claim" => {
                let blocked = (first..first + count).find_map(|doc_id| {
                    coordinator
                        .try_claim(session, &claims(doc_id, strength))
                        .unwrap()
                        .err()
                });
                match blocked {
                    Some(claim) => respond(format!("conflict {} {}", claim.offset, claim.write)),
                    None => respond("granted"),
                }
            }
            "release" => {
                let all = (first..first + count)
                    .flat_map(|doc_id| claims(doc_id, strength))
                    .collect::<Vec<_>>();
                coordinator.release(session, &all);
                respond("released");
            }
            _ => panic!("unexpected row claim peer command {line}"),
        }
    }
}

fn index(strength: LockStrength) -> usize {
    STRENGTHS
        .iter()
        .position(|candidate| *candidate == strength)
        .unwrap()
}

#[test]
fn mapped_and_positioned_processes_share_claim_publication_and_release() {
    for parent_mapped in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mixed.db");
        let coordinator = FileLockCoordinator::open(&path).unwrap();
        if !parent_mapped {
            coordinator.claim_mapping.lock().disable();
        }
        let mut peer = Peer::start_with_mapping(&path, !parent_mapped);
        claim(&coordinator, PARENT_SESSION, 1, LockStrength::ForUpdate);
        assert_eq!(peer.request("claim 3 2"), "granted");
        assert!(peer.request("claim 3 1").starts_with("conflict "));
        assert!(coordinator
            .try_claim(PARENT_SESSION, &claims(2, LockStrength::ForUpdate))
            .unwrap()
            .is_err());
        coordinator.release(PARENT_SESSION, &claims(1, LockStrength::ForUpdate));
        assert_eq!(peer.request("claim 3 1"), "granted");
        assert_eq!(peer.request("release 3 2"), "released");
        claim(&coordinator, PARENT_SESSION, 2, LockStrength::ForUpdate);
        peer.terminate();
        claim(&coordinator, PARENT_SESSION, 1, LockStrength::ForUpdate);
    }
}

#[test]
fn the_tuple_lock_matrix_holds_between_processes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("matrix.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let mut peer = Peer::start(&path);
    for held in STRENGTHS {
        assert_eq!(peer.request(&format!("claim {} 1", index(held))), "granted");
        for wanted in STRENGTHS {
            let result = coordinator
                .try_claim(PARENT_SESSION, &claims(1, wanted))
                .unwrap();
            assert_eq!(
                result.is_err(),
                lock_strengths_conflict(held, wanted),
                "{held:?} {wanted:?}"
            );
            if let Err(blocked) = result {
                // The reported claim is one the request made, and the peer's session holds it.
                assert!(claims(1, wanted).contains(&blocked), "{held:?} {wanted:?}");
                let holders = coordinator.foreign_row_holders(blocked);
                assert_eq!(holders.len(), 1);
                assert_eq!(holders[0].session, PEER_SESSION);
                assert_eq!(holders[0].pid, peer.child.id());
                assert_eq!(holders[0].offset, blocked.offset);
            } else {
                coordinator.release(PARENT_SESSION, &claims(1, wanted));
            }
            // A row the peer does not hold is never blocked.
            claim(&coordinator, PARENT_SESSION, 2, wanted);
            coordinator.release(PARENT_SESSION, &claims(2, wanted));
        }
        assert_eq!(
            peer.request(&format!("release {} 1", index(held))),
            "released"
        );
    }
    assert!(stored(&coordinator).1.is_empty());
}

#[test]
fn a_blocked_claim_leaves_nothing_behind_and_succeeds_after_the_release() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("blocked.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let mut peer = Peer::start(&path);
    claim(&coordinator, PARENT_SESSION, 1, LockStrength::ForShare);
    assert_eq!(peer.request("claim 1 1"), "granted");
    let update = claims(1, LockStrength::ForUpdate);
    // The parent's own shared claim does not hide the peer's: the upgrade is blocked on the row byte.
    assert_eq!(
        coordinator.try_claim(PARENT_SESSION, &update),
        Ok(Err(update[1]))
    );
    assert_eq!(
        modes(&coordinator),
        [
            (PARENT_SESSION, Mode::None, Mode::Shared),
            (PEER_SESSION, Mode::None, Mode::Shared)
        ]
    );
    assert_eq!(
        coordinator
            .state
            .lock()
            .rows
            .counts(PARENT_SESSION, row_claim_address(update[0]).unwrap().0),
        Counts {
            row_shared: 1,
            ..Counts::default()
        }
    );
    assert_eq!(
        peer.request("claim 3 1"),
        format!("conflict {} true", update[1].offset)
    );
    assert_eq!(peer.request("release 1 1"), "released");
    assert_eq!(coordinator.try_claim(PARENT_SESSION, &update), Ok(Ok(())));
    assert_eq!(
        modes(&coordinator),
        [(PARENT_SESSION, Mode::Exclusive, Mode::Exclusive)]
    );
}

#[test]
fn the_claims_of_a_killed_process_are_discarded() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("killed.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let mut peer = Peer::start(&path);
    assert_eq!(peer.request("claim 3 1 500"), "granted");
    let update = claims(250, LockStrength::ForUpdate);
    assert!(coordinator
        .try_claim(PARENT_SESSION, &update)
        .unwrap()
        .is_err());
    peer.terminate();
    assert!(coordinator.foreign_row_holders(update[0]).is_empty());
    assert_eq!(coordinator.try_claim(PARENT_SESSION, &update), Ok(Ok(())));
    // Only the dead claim that blocked was removed; the others block nothing and stay until the table is rebuilt.
    let entries = stored(&coordinator).1;
    assert_eq!(entries.len(), 500);
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.session == PARENT_SESSION)
            .count(),
        1
    );
    for doc_id in 1000..3200 {
        claim(
            &coordinator,
            PARENT_SESSION,
            doc_id,
            LockStrength::ForUpdate,
        );
    }
    let (header, entries) = stored(&coordinator);
    assert!(header.capacity_log2 > table::INITIAL_CAPACITY_LOG2);
    assert_eq!(entries.len(), 2201);
    assert!(entries.iter().all(|entry| entry.session == PARENT_SESSION));
}

#[test]
fn growing_the_table_keeps_the_claims_of_every_process() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("growth.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let mut peer = Peer::start(&path);
    assert_eq!(peer.request("claim 3 1 10"), "granted");
    for doc_id in 100..6100 {
        claim(
            &coordinator,
            PARENT_SESSION,
            doc_id,
            LockStrength::ForUpdate,
        );
    }
    let (header, entries) = stored(&coordinator);
    assert!(header.capacity_log2 > table::INITIAL_CAPACITY_LOG2);
    assert_eq!(entries.len(), 6010);
    for doc_id in 1..=10 {
        assert!(coordinator
            .try_claim(PARENT_SESSION, &claims(doc_id, LockStrength::ForKeyShare))
            .unwrap()
            .is_err());
    }
    // The peer reads the rebuilt table and still finds the parent's claims.
    assert!(peer.request("claim 0 100 6000").starts_with("conflict "));
    assert_eq!(peer.request("claim 0 6100 3000"), "granted");

    // Another process is attached, so releasing everything does not reset the table.
    let all = (100..6100)
        .flat_map(|doc_id| claims(doc_id, LockStrength::ForUpdate))
        .collect::<Vec<_>>();
    coordinator.release(PARENT_SESSION, &all);
    let (released, entries) = stored(&coordinator);
    assert_eq!(
        released.capacity_log2,
        header.capacity_log2.max(released.capacity_log2)
    );
    assert_eq!(entries.len(), 3010);
    assert_eq!(peer.request("release 0 6100 3000"), "released");
    assert_eq!(peer.request("release 3 1 10"), "released");
    assert!(stored(&coordinator).1.is_empty());
}
