//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! OS process death releases the exclusive owner without changing authoritative durable receipts.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use super::*;

const PATH_ENV: &str = "UQA_REDB_SERIALIZABLE_RECOVERY_TEST_PATH";

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn killed_database_owner_recovers_live_and_prepared_participants_from_receipts() {
    if let Some(path) = std::env::var_os(PATH_ENV) {
        let store = RedbRecordStore::new(Arc::new(Database::create(path).unwrap())).unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let active = actor(&store, &control);
        let pending = actor(&store, &control);
        let committed = actor(&store, &control);
        let (pending_write, _) = prepare(&store, &pending, b"pending", &control);
        let (publication, prepared) = prepare(&store, &committed, b"committed", &control);
        graph(&store, &control, |_| {
            store
                .commit(publication.transaction(), &prepared, &control)
                .unwrap();
            Ok(())
        })
        .unwrap();
        println!(
            "ready:{}:{}",
            pending_write.transaction().allocation(),
            publication.transaction().allocation()
        );
        std::io::stdout().flush().unwrap();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).unwrap();
        drop((active, pending, committed));
        panic!("the parent must terminate this process with its actors retained");
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("process.redb");
    let test = concat!(
        module_path!(),
        "::killed_database_owner_recovers_live_and_prepared_participants_from_receipts"
    );
    let (_, test) = test.split_once("::").unwrap();
    let mut process = Process(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(PATH_ENV, &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let output = process.0.stdout.take().unwrap();
    let (send, events) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            if send.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let identifiers = loop {
        let event = events.recv_timeout(Duration::from_secs(60)).unwrap();
        if let Some(ids) = event.strip_prefix("ready:") {
            break ids.to_owned();
        }
    };
    assert!(Database::open(&path).is_err());
    process.0.kill().unwrap();
    assert!(!process.0.wait().unwrap().success());
    reader.join().unwrap();
    let (pending, committed) = identifiers.split_once(':').unwrap();
    let store = RedbRecordStore::new(Arc::new(Database::open(&path).unwrap())).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let pending = StorageTransactionId::new(store.identity, pending.parse().unwrap()).unwrap();
    let committed = StorageTransactionId::new(store.identity, committed.parse().unwrap()).unwrap();
    let (next, view) = store.admit_serializable_snapshot(true, &control).unwrap();
    assert_eq!(next.id().allocation(), 4);
    assert_eq!(
        store.commit_status(pending, &control).unwrap(),
        CommitStatus::Aborted
    );
    assert!(matches!(
        store.commit_status(committed, &control).unwrap(),
        CommitStatus::Committed(_)
    ));
    assert!(view.get(b"pending", &control).unwrap().is_none());
    assert_eq!(
        view.get(b"committed", &control)
            .unwrap()
            .unwrap()
            .value()
            .map(|value| &***value),
        Some(&b"durable"[..])
    );
    drop((next, view));
    store.recover_serializable_participants(&control).unwrap();
    assert_eq!(control.memory().used(), 0);
}
