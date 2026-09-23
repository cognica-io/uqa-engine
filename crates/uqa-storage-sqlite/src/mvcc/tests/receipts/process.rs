//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native process loss releases managed resolution ownership without acknowledging raw IDs.

use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};
use uqa_storage::mvcc::receipt_lease_id;

const PATH_ENV: &str = "UQA_RECEIPT_OWNER_TEST_PATH";
const MODE_ENV: &str = "UQA_RECEIPT_OWNER_TEST_MODE";
const STAGE_ENV: &str = "UQA_RECEIPT_OWNER_TEST_STAGE";

struct Peer(Child);
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn block() {
    println!("receipt-owner-ready");
    std::io::stdout().flush().unwrap();
    let mut command = String::new();
    std::io::stdin().read_line(&mut command).unwrap();
    panic!("receipt process must be killed at its retained boundary");
}

fn child(path: &std::path::Path, mode: usize, stage: usize) {
    let connection = open(path, mode);
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    if stage == 0 {
        store
            .with_receipt_admission(&control, |leases| {
                let mut owner = None;
                store.with_write(&control, |connection| {
                    crate::mvcc::write::allocate_with_owner(
                        connection,
                        store.identity,
                        store.native,
                        true,
                        &control,
                        |id| {
                            owner = Some(leases.retain(receipt_lease_id(id), &control)?);
                            block();
                            Ok(())
                        },
                    )
                })?;
                drop(owner);
                Ok(())
            })
            .unwrap();
    } else {
        let owner = store.allocate_managed_transaction(&control).unwrap();
        if stage == 2 {
            store
                .commit(
                    owner.transaction(),
                    &prepared(b"survives-owner", b"durable", &control),
                    &control,
                )
                .unwrap();
        }
        block();
        drop(owner);
    }
}

#[test]
fn process_loss_before_and_after_pending_publication_releases_only_managed_receipts() {
    if let Some(path) = std::env::var_os(PATH_ENV) {
        child(
            std::path::Path::new(&path),
            std::env::var(MODE_ENV).unwrap().parse().unwrap(),
            std::env::var(STAGE_ENV).unwrap().parse().unwrap(),
        );
        return;
    }
    for mode in 0..4 {
        for stage in 0..3 {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("process.db");
            let connection = open(&path, mode);
            let store = SQLiteRecordStore::new(&connection).unwrap();
            let control = control();
            let manual = store.allocate_transaction(&control).unwrap();
            let (_, test) = concat!(module_path!(), "::process_loss_before_and_after_pending_publication_releases_only_managed_receipts").split_once("::").unwrap();
            let mut peer = Peer(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", test, "--nocapture"])
                    .env(PATH_ENV, &path)
                    .env(MODE_ENV, mode.to_string())
                    .env(STAGE_ENV, stage.to_string())
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .unwrap(),
            );
            let output = peer.0.stdout.take().unwrap();
            let (send, receive) = mpsc::channel();
            let reader = std::thread::spawn(move || {
                for line in BufReader::new(output).lines() {
                    if line.unwrap() == "receipt-owner-ready" {
                        let _ = send.send(());
                        return;
                    }
                }
            });
            receive
                .recv_timeout(Duration::from_secs(30))
                .expect("receipt owner did not reach boundary");
            if stage != 0 {
                assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
            }
            peer.0.kill().unwrap();
            assert!(!peer.0.wait().unwrap().success());
            reader.join().unwrap();
            assert_eq!(
                store.reclaim_transaction_receipts(&control).unwrap(),
                u64::from(stage != 0)
            );
            assert_eq!(
                store.commit_status(manual, &control).unwrap(),
                CommitStatus::Pending
            );
            assert_eq!(
                store
                    .snapshot(&control)
                    .unwrap()
                    .get(b"survives-owner", &control)
                    .unwrap()
                    .is_some(),
                stage == 2
            );
            let next = store.allocate_managed_transaction(&control).unwrap();
            assert_eq!(
                next.transaction().allocation(),
                if stage == 0 { 2 } else { 3 }
            );
            assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
            drop(next);
            assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
        }
    }
}
