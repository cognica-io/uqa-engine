//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live native leases protect detached readers; closing a view or killing its process releases retention.

use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

const PATH_ENV: &str = "UQA_SNAPSHOT_LEASE_TEST_PATH";
const MODE_ENV: &str = "UQA_SNAPSHOT_LEASE_TEST_MODE";

struct Peer {
    child: Child,
    events: mpsc::Receiver<String>,
}

impl Peer {
    fn start(path: &Path, mode: usize) -> Self {
        let (_, test) = concat!(
            module_path!(),
            "::live_and_dead_processes_define_the_retention_horizon"
        )
        .split_once("::")
        .unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(PATH_ENV, path)
            .env(MODE_ENV, mode.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (send, events) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if send.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let peer = Self { child, events };
        peer.expect("lease-ready");
        peer
    }

    fn expect(&self, expected: &str) {
        loop {
            let event = self
                .events
                .recv_timeout(Duration::from_secs(30))
                .expect("snapshot peer did not reach the expected barrier");
            if event == expected {
                return;
            }
        }
    }

    fn command(&mut self, command: &str, expected: &str) {
        let input = self.child.stdin.as_mut().unwrap();
        writeln!(input, "{command}").unwrap();
        input.flush().unwrap();
        self.expect(expected);
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn event(value: &str) {
    println!("{value}");
    std::io::stdout().flush().unwrap();
}

fn child(path: &Path, mode: usize) {
    let connection = open(path, mode);
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let mut snapshot = Some(store.snapshot(&control).unwrap());
    event("lease-ready");
    for line in std::io::stdin().lock().lines() {
        match line.unwrap().as_str() {
            "check" => {
                assert_eq!(
                    &***snapshot
                        .as_ref()
                        .unwrap()
                        .get(b"key", &control)
                        .unwrap()
                        .unwrap()
                        .value()
                        .unwrap(),
                    b"old"
                );
                event("old-visible");
            }
            "release" => {
                snapshot.take();
                event("lease-released");
            }
            "quit" => {
                event("peer-finished");
                return;
            }
            other => panic!("unexpected peer command {other}"),
        }
    }
}

#[test]
fn live_and_dead_processes_define_the_retention_horizon() {
    if let Some(path) = std::env::var_os(PATH_ENV) {
        child(
            Path::new(&path),
            std::env::var(MODE_ENV).unwrap().parse().unwrap(),
        );
        return;
    }
    for mode in 0..4 {
        for killed in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("peer.db");
            let connection = open(&path, mode);
            let store = SQLiteRecordStore::new(&connection).unwrap();
            let control = control();
            let id = store.allocate_transaction(&control).unwrap();
            let original = store
                .commit(id, &prepared(b"key", b"old", &control), &control)
                .unwrap();
            let mut peer = Peer::start(&path, mode);
            let changed = PreparedRecordCommit::new(
                &[RecordWrite {
                    key: b"key",
                    expected: Some(original.sequence),
                    value: Some(b"new"),
                }],
                &control,
            )
            .unwrap();
            let id = store.allocate_transaction(&control).unwrap();
            store.commit(id, &changed, &control).unwrap();
            assert_eq!(store.reclaim_versions(&control).unwrap(), 0);
            peer.command("check", "old-visible");
            if killed {
                peer.child.kill().unwrap();
                assert!(!peer.child.wait().unwrap().success());
            } else {
                peer.command("release", "lease-released");
            }
            assert_eq!(store.reclaim_versions(&control).unwrap(), 1);
            let current = store.snapshot(&control).unwrap();
            assert_eq!(
                &***current
                    .get(b"key", &control)
                    .unwrap()
                    .unwrap()
                    .value()
                    .unwrap(),
                b"new"
            );
            if !killed {
                peer.command("quit", "peer-finished");
                assert!(peer.child.wait().unwrap().success());
            }
        }
    }
}
