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
use redb::backends::FileBackend;

const PATH_ENV: &str = "UQA_REDB_SERIALIZABLE_RECOVERY_TEST_PATH";
const POINT_ENV: &str = "UQA_REDB_SERIALIZABLE_RECOVERY_TEST_POINT";

#[derive(Debug)]
struct SyncGate {
    file: FileBackend,
    armed: Arc<AtomicBool>,
    after_sync: bool,
}

impl StorageBackend for SyncGate {
    fn len(&self) -> io::Result<u64> {
        self.file.len()
    }
    fn read(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
        self.file.read(offset, out)
    }
    fn set_len(&self, length: u64) -> io::Result<()> {
        self.file.set_len(length)
    }
    fn write(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.file.write(offset, bytes)
    }
    fn sync_data(&self) -> io::Result<()> {
        if self.armed.swap(false, Ordering::AcqRel) {
            if self.after_sync {
                self.file.sync_data()?;
            }
            println!("commit-sync");
            std::io::stdout().flush()?;
            loop {
                std::thread::park();
            }
        }
        self.file.sync_data()
    }
}

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
        let point: usize = std::env::var(POINT_ENV).unwrap().parse().unwrap();
        let armed = Arc::new(AtomicBool::new(false));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        let database = Database::builder()
            .create_with_backend(SyncGate {
                file: FileBackend::new(file).unwrap(),
                armed: Arc::clone(&armed),
                after_sync: point == 2,
            })
            .unwrap();
        let store = RedbRecordStore::new(Arc::new(database)).unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let active = actor(&store, &control);
        let pending = actor(&store, &control);
        let committed = actor(&store, &control);
        let (pending_write, _) = prepare(&store, &pending, b"pending", &control);
        let records =
            [b"committed".as_slice(), b"committed-peer".as_slice()].map(|key| RecordWrite {
                key,
                expected: None,
                value: Some(b"durable"),
            });
        let (publication, prepared) = prepare_records(&store, &committed, &records, &control);
        println!(
            "ready:{}:{}",
            pending_write.transaction().allocation(),
            publication.transaction().allocation()
        );
        std::io::stdout().flush().unwrap();
        graph(&store, &control, |_| {
            armed.store(point != 0, Ordering::Release);
            store
                .commit(publication.transaction(), &prepared, &control)
                .unwrap();
            Ok(())
        })
        .unwrap();
        println!("records-committed");
        std::io::stdout().flush().unwrap();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).unwrap();
        drop((active, pending, committed));
        panic!("the parent must terminate this process with its actors retained");
    }
    for point in 0..3 {
        recover_at(point);
    }
}

fn recover_at(point: usize) {
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
            .env(POINT_ENV, point.to_string())
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
    let barrier = if point == 0 {
        "records-committed"
    } else {
        "commit-sync"
    };
    loop {
        if events.recv_timeout(Duration::from_secs(60)).unwrap() == barrier {
            break;
        }
    }
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
    let outcome = store.commit_status(committed, &control).unwrap();
    if point == 0 {
        assert!(matches!(outcome, CommitStatus::Committed(_)));
    }
    assert!(view.get(b"pending", &control).unwrap().is_none());
    for key in [b"committed".as_slice(), b"committed-peer".as_slice()] {
        let record = view.get(key, &control).unwrap();
        match outcome {
            CommitStatus::Committed(_) => {
                assert_eq!(
                    record.unwrap().value().map(|value| &***value),
                    Some(&b"durable"[..])
                );
            }
            CommitStatus::Aborted => assert!(record.is_none()),
            other => panic!("unresolved physical outcome at point {point}: {other:?}"),
        }
    }
    drop((next, view));
    store.recover_serializable_participants(&control).unwrap();
    assert_eq!(control.memory().used(), 0);
}
