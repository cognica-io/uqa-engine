//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent process leases protect metadata epoch zero and disappear after process death.

use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

const PATH_ENV: &str = "UQA_BACKUP_METADATA_LEASE_PATH";
const MODE_ENV: &str = "UQA_BACKUP_METADATA_LEASE_MODE";

struct Peer {
    child: Child,
    events: mpsc::Receiver<String>,
}

impl Peer {
    fn open(path: &Path, mode: usize) -> Self {
        let test = concat!(
            module_path!(),
            "::metadata_leases_survive_other_processes_and_release_after_exit"
        )
        .split_once("::")
        .unwrap()
        .1;
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
        peer.expect("captured");
        peer
    }

    fn expect(&self, expected: &str) {
        loop {
            let line = self
                .events
                .recv_timeout(Duration::from_secs(30))
                .expect("metadata peer must reach its barrier");
            if line == expected {
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

fn event(message: &str) {
    println!("{message}");
    std::io::stdout().flush().unwrap();
}

fn child(path: &Path, mode: usize) {
    let connection = super::super::reclamation::open(path, mode);
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let mut snapshot = Some(capture(&store, &control));
    assert_eq!(
        snapshot.as_ref().unwrap().metadata_epoch(),
        MetadataEpoch::INITIAL
    );
    event("captured");
    for line in std::io::stdin().lock().lines() {
        match line.unwrap().as_str() {
            "verify" => {
                let snapshot = snapshot.as_ref().unwrap();
                assert_eq!(
                    watermarks(snapshot.as_ref(), &control),
                    vec![(b"a".to_vec(), 7)]
                );
                assert_eq!(
                    &***snapshot
                        .records()
                        .get(b"row", &control)
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
                event("released");
            }
            "exit" => {
                event("finished");
                return;
            }
            command => panic!("unexpected metadata peer command {command}"),
        }
    }
}

#[test]
fn metadata_leases_survive_other_processes_and_release_after_exit() {
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
            let path = directory.path().join("process.db");
            let connection = super::super::reclamation::open(&path, mode);
            let store = SQLiteRecordStore::new(&connection).unwrap();
            let control = control();
            observe(&store, b"a", 7, &control);
            let sequence = write(&store, b"old", None, &control);
            let mut peer = Peer::open(&path, mode);
            observe(&store, b"a", 8, &control);
            write(&store, b"new", Some(sequence), &control);
            let second = capture(&store, &control);
            observe(&store, b"a", 9, &control);
            drop(second);
            assert_eq!(store.reclaim_versions(&control).unwrap(), 0);
            assert_eq!(history_len(&store), 2);
            peer.command("verify", "old-visible");
            if killed {
                peer.child.kill().unwrap();
                assert!(!peer.child.wait().unwrap().success());
            } else {
                peer.command("release", "released");
            }
            assert_eq!(store.reclaim_versions(&control).unwrap(), 1);
            assert_eq!(history_len(&store), 0);
            assert_eq!(store.identifier_watermark(b"a", &control).unwrap(), Some(9));
            if !killed {
                peer.command("exit", "finished");
                assert!(peer.child.wait().unwrap().success());
            }
        }
    }
}
