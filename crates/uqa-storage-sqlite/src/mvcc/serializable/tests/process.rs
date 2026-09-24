//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Process handoff preserves conflicts; process death rolls back an unfinished checkpoint.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use super::*;

mod liveness;

const PATH_ENV: &str = "UQA_SERIALIZABLE_TRANSPORT_TEST_PATH";
const MODE_ENV: &str = "UQA_SERIALIZABLE_TRANSPORT_TEST_MODE";

struct Peer {
    child: Child,
    events: mpsc::Receiver<String>,
}

impl Peer {
    fn start(path: &Path, mode: usize) -> Self {
        Self::start_test(
            path,
            mode,
            concat!(
                module_path!(),
                "::independent_processes_keep_dependencies_and_release_abandoned_admission"
            ),
            "read-retained",
        )
    }

    fn start_test(path: &Path, mode: usize, test: &str, ready: &str) -> Self {
        let (_, test) = test.split_once("::").unwrap();
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
        peer.expect(ready);
        peer
    }

    fn expect_prefix(&self, prefix: &str) -> String {
        loop {
            let event = self
                .events
                .recv_timeout(Duration::from_secs(30))
                .expect("serializable peer did not publish its participant identities");
            if let Some(value) = event.strip_prefix(prefix) {
                return value.to_owned();
            }
        }
    }

    fn expect(&self, expected: &str) {
        loop {
            let event = self
                .events
                .recv_timeout(Duration::from_secs(30))
                .expect("serializable peer did not reach the expected barrier");
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
    let actor = admit_read(&store, b"right");
    let control = control();
    event("read-retained");
    let mut unfinished = None;
    for command in std::io::stdin().lock().lines() {
        match command.unwrap().as_str() {
            "conflict" => {
                let mut held = store.serializable_admission(&control).unwrap();
                assert!(matches!(
                    held.graph_mut()
                        .observe_write(actor, point(b"left"), &control),
                    Err(VersionError::SerializationConflict { .. })
                ));
                held.graph_mut().rollback(actor).unwrap();
                held.persist(&control).unwrap();
                event("conflict-retained");
            }
            "hold" => {
                let mut held = store.serializable_admission(&control).unwrap();
                held.graph_mut().admit(true, &control).unwrap();
                // Modify the physical BLOB too; killing the process must recover its journal.
                held.connection
                    .execute("UPDATE _uqa_serializable_records SET value = x'00'", [])
                    .unwrap();
                held.connection.cache_flush().unwrap();
                unfinished = Some(held);
                event("checkpoint-uncommitted");
            }
            other => panic!("unexpected serializable peer command {other}"),
        }
    }
    drop(unfinished);
}

#[test]
fn independent_processes_keep_dependencies_and_release_abandoned_admission() {
    if let Some(path) = std::env::var_os(PATH_ENV) {
        child(
            Path::new(&path),
            std::env::var(MODE_ENV).unwrap().parse().unwrap(),
        );
        return;
    }
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peer.db");
        let connection = open(&path, mode);
        let store = SQLiteRecordStore::new(&connection).unwrap();
        let actor = admit_read(&store, b"left");
        let mut peer = Peer::start(&path, mode);
        let control = control();
        let mut held = store.serializable_admission(&control).unwrap();
        held.graph_mut()
            .observe_write(actor, point(b"right"), &control)
            .unwrap();
        held.graph_mut().prepare_commit(actor, &control).unwrap();
        held.graph_mut().commit(actor).unwrap();
        held.persist(&control).unwrap();
        peer.command("conflict", "conflict-retained");
        peer.command("hold", "checkpoint-uncommitted");
        peer.child.kill().unwrap();
        assert!(!peer.child.wait().unwrap().success());
        let mut held = store.serializable_admission(&control).unwrap();
        assert!(matches!(
            held.graph().check_active(actor),
            Err(VersionError::TransactionFinished)
        ));
        let next = held.graph_mut().admit(true, &control).unwrap();
        assert_eq!(
            next.allocation(),
            3,
            "the abandoned allocation was not published"
        );
        held.persist(&control).unwrap();
    }
}
