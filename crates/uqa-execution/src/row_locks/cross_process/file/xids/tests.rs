//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transaction ID reservations, the shared cursor, and its attachment across restarts and processes.

use std::io::{BufRead, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};

use super::super::{
    read_exact_at, FileLockCoordinator, TRANSACTION_XID_CURSOR_OFFSET, TRANSACTION_XID_CURSOR_SIZE,
    TRANSACTION_XID_STATE_MAGIC, TRANSACTION_XID_STATE_OFFSET, TRANSACTION_XID_STATE_VERSION,
};

const RESPONSE_PREFIX: &str = "UQA_TRANSACTION_XID_RESPONSE ";

fn sidecar(database: &Path) -> PathBuf {
    let mut sidecar = database.as_os_str().to_owned();
    sidecar.push(".uqa-locks");
    sidecar.into()
}

fn allocate(coordinator: &FileLockCoordinator) -> u32 {
    coordinator.allocate_transaction_xid().unwrap().unwrap()
}

fn limit(coordinator: &FileLockCoordinator) -> u32 {
    let length = coordinator.file.metadata().unwrap().len();
    coordinator.transaction_xid_limit(length).unwrap()
}

/// A sidecar as a coordinator without reservations left it: only the state, holding its next transaction ID.
fn write_state(database: &Path, magic: u32, next: u64) {
    let mut image = vec![0_u8; usize::try_from(TRANSACTION_XID_STATE_OFFSET).unwrap()];
    image.extend_from_slice(&magic.to_be_bytes());
    image.extend_from_slice(&TRANSACTION_XID_STATE_VERSION.to_be_bytes());
    image.extend_from_slice(&next.to_be_bytes());
    std::fs::write(sidecar(database), image).unwrap();
}

#[test]
fn reservations_double_and_bound_every_transaction_id() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("xids.db")).unwrap();
    let mut limits = Vec::new();
    for expected in 3..2003 {
        assert_eq!(allocate(&coordinator), expected);
        let limit = limit(&coordinator);
        assert!(expected < limit);
        if limits.last() != Some(&limit) {
            limits.push(limit);
        }
    }
    assert_eq!(limits, [4, 6, 10, 18, 34, 66, 130, 258, 514, 1026, 2050]);
    assert_eq!(allocate(&coordinator), 2003);
    for expected in 2004..2051 {
        assert_eq!(allocate(&coordinator), expected);
    }
    // A reservation never grows past its maximum.
    assert_eq!(limit(&coordinator), 2050 + 1024);
}

#[test]
fn an_orderly_restart_continues_at_the_next_transaction_id() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("xids.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    for expected in 3..8 {
        assert_eq!(allocate(&coordinator), expected);
    }
    assert_eq!(limit(&coordinator), 10);
    drop(coordinator);
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    assert_eq!(limit(&coordinator), 8);
    assert_eq!(allocate(&coordinator), 8);
    assert_eq!(allocate(&coordinator), 9);
}

#[test]
fn a_restart_without_detaching_resumes_at_the_durable_limit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("xids.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    for expected in 3..8 {
        assert_eq!(allocate(&coordinator), expected);
    }
    let end = usize::try_from(TRANSACTION_XID_CURSOR_OFFSET).unwrap() + TRANSACTION_XID_CURSOR_SIZE;
    let mut image = vec![0_u8; end];
    read_exact_at(&coordinator.file, &mut image, 0).unwrap();
    drop(coordinator);
    // The sidecar as a process that ended without detaching leaves it: the durable limit with a cursor inside its reservation.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(sidecar(&path))
        .unwrap();
    file.rewind().unwrap();
    file.write_all(&image).unwrap();
    drop(file);
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    assert_eq!(limit(&coordinator), 10);
    assert_eq!(allocate(&coordinator), 10);
    assert_eq!(allocate(&coordinator), 11);
}

#[test]
fn a_sidecar_without_reservations_continues_at_its_next_transaction_id() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("xids.db");
    write_state(&path, TRANSACTION_XID_STATE_MAGIC, 500);
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    assert_eq!(allocate(&coordinator), 500);
    assert_eq!(limit(&coordinator), 501);
    assert_eq!(allocate(&coordinator), 501);
}

#[test]
fn transaction_ids_start_over_after_the_last_one() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("xids.db");
    write_state(&path, TRANSACTION_XID_STATE_MAGIC, u64::from(u32::MAX) - 1);
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    assert_eq!(allocate(&coordinator), u32::MAX - 1);
    assert_eq!(allocate(&coordinator), u32::MAX);
    assert_eq!(limit(&coordinator), 3);
    assert_eq!(allocate(&coordinator), 3);
    assert_eq!(allocate(&coordinator), 4);
}

#[test]
fn a_corrupt_state_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    for (name, magic, next) in [
        ("magic.db", TRANSACTION_XID_STATE_MAGIC + 1, 500),
        ("below.db", TRANSACTION_XID_STATE_MAGIC, 2),
        (
            "above.db",
            TRANSACTION_XID_STATE_MAGIC,
            u64::from(u32::MAX) + 1,
        ),
    ] {
        let path = directory.path().join(name);
        write_state(&path, magic, next);
        let coordinator = FileLockCoordinator::open(&path).unwrap();
        assert_eq!(
            coordinator.allocate_transaction_xid().unwrap_err(),
            "transaction XID allocator state is corrupt",
            "{name}"
        );
    }
}

#[test]
fn attached_processes_share_one_cursor() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("xids.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    for expected in 3..6 {
        assert_eq!(allocate(&coordinator), expected);
    }
    let mut peer = Peer::start(&path);
    assert_eq!(peer.request("allocate"), "6");
    assert_eq!(allocate(&coordinator), 7);
    assert_eq!(peer.request("allocate"), "8");
    // The failure of a process loses nothing the cursor recorded.
    peer.terminate();
    assert_eq!(allocate(&coordinator), 9);
}

#[test]
fn the_last_attached_process_alone_returns_or_discards_the_reservation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("xids.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    for expected in 3..6 {
        assert_eq!(allocate(&coordinator), expected);
    }
    let mut peer = Peer::start(&path);
    for expected in 6..10 {
        assert_eq!(peer.request("allocate"), expected.to_string());
    }
    // The peer is still attached, so this coordinator returns nothing.
    let reserved = limit(&coordinator);
    assert_eq!(reserved, 13);
    drop(coordinator);
    assert_eq!(peer.request("allocate"), "10");
    // Nothing vouches for the cursor once the only attached process has failed.
    peer.terminate();
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    assert_eq!(limit(&coordinator), reserved);
    assert_eq!(allocate(&coordinator), reserved);
}

struct Peer {
    child: Child,
    responses: std::sync::mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Peer {
    fn start(path: &Path) -> Self {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "row_locks::cross_process::file::xids::tests::transaction_xid_peer",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("UQA_TRANSACTION_XID_TEST_PATH", path)
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
                    "transaction ID peer response failed: {error}; status {:?}",
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

#[test]
#[ignore = "subprocess entry point for transaction ID allocation tests"]
fn transaction_xid_peer() {
    let path = std::env::var_os("UQA_TRANSACTION_XID_TEST_PATH").unwrap();
    let coordinator = FileLockCoordinator::open(Path::new(&path)).unwrap();
    respond("ready");
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        match line.as_str() {
            "allocate" => respond(allocate(&coordinator)),
            _ => panic!("unexpected transaction ID peer command {line}"),
        }
    }
}
