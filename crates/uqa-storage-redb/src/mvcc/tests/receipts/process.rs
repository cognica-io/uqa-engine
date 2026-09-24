//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Process death releases managed receipt owners without changing committed data or manual receipts.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use super::*;

const PATH_ENV: &str = "UQA_REDB_RECEIPT_RECOVERY_TEST_PATH";

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn retain_child_receipts(path: std::ffi::OsString) -> ! {
    let store = RedbRecordStore::new(Arc::new(Database::create(path).unwrap())).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    store.set_receipt_retention_limit(4, &control).unwrap();
    let pending = store.allocate_managed_transaction(&control).unwrap();
    let committed = store.allocate_managed_transaction(&control).unwrap();
    let aborted = store.allocate_managed_transaction(&control).unwrap();
    let manual = store.allocate_transaction(&control).unwrap();
    let prepared = PreparedRecordCommit::new(
        &[RecordWrite {
            key: b"committed",
            expected: None,
            value: Some(b"survives-owner-death"),
        }],
        &control,
    )
    .unwrap();
    store
        .commit(committed.transaction(), &prepared, &control)
        .unwrap();
    store.abort(aborted.transaction(), &control).unwrap();
    println!(
        "ready:{}:{}:{}:{}",
        pending.transaction().allocation(),
        committed.transaction().allocation(),
        aborted.transaction().allocation(),
        manual.allocation()
    );
    std::io::stdout().flush().unwrap();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    drop((pending, committed, aborted));
    panic!("the parent must terminate this process with its receipt owners retained");
}

#[test]
fn killed_database_owner_releases_only_managed_receipts_and_preserves_committed_records() {
    if let Some(path) = std::env::var_os(PATH_ENV) {
        retain_child_receipts(path);
    }

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("managed-receipts.redb");
    let test = concat!(
        module_path!(),
        "::killed_database_owner_releases_only_managed_receipts_and_preserves_committed_records"
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
        if let Some(identifiers) = event.strip_prefix("ready:") {
            break identifiers.to_owned();
        }
    };
    assert!(Database::open(&path).is_err());
    process.0.kill().unwrap();
    assert!(!process.0.wait().unwrap().success());
    reader.join().unwrap();
    let store = RedbRecordStore::new(Arc::new(Database::open(&path).unwrap())).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let ids: Vec<_> = identifiers
        .split(':')
        .map(|allocation| {
            StorageTransactionId::new(store.identity, allocation.parse().unwrap()).unwrap()
        })
        .collect();
    assert_eq!(ids.len(), 4);
    assert_eq!(
        store.commit_status(ids[0], &control).unwrap(),
        CommitStatus::Pending
    );
    assert!(matches!(
        store.commit_status(ids[1], &control).unwrap(),
        CommitStatus::Committed(_)
    ));
    assert_eq!(
        store.commit_status(ids[2], &control).unwrap(),
        CommitStatus::Aborted
    );
    assert!(matches!(
        store.allocate_transaction(&control),
        Err(VersionError::ReceiptRetentionExhausted { limit: 4 })
    ));
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 3);
    for id in &ids[..3] {
        assert_eq!(
            store.commit_status(*id, &control).unwrap(),
            CommitStatus::Unknown
        );
    }
    assert_eq!(
        store.commit_status(ids[3], &control).unwrap(),
        CommitStatus::Pending
    );
    let snapshot = store.snapshot(&control).unwrap();
    assert_eq!(
        snapshot
            .get(b"committed", &control)
            .unwrap()
            .unwrap()
            .value()
            .map(|value| &***value),
        Some(b"survives-owner-death".as_slice())
    );
    assert!(store.allocate_transaction(&control).unwrap().allocation() > ids[3].allocation());
}
