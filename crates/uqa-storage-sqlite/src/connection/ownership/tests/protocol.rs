//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Predecessor owners use the original slot layout and tag, not a simulated new-format claim table.

use std::io::{BufRead, Write};
use std::process::{Child, Stdio};

use super::*;
use crate::mvcc::leases::{LeaseNamespace, NativeLeaseFile};
use uqa_storage::mvcc::DatabaseId;

fn owner_file(path: &Path) -> NativeLeaseFile {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".uqa-owners");
    NativeLeaseFile::open(
        Path::new(&sidecar),
        LeaseNamespace {
            magic: *b"UQAOWN01",
            database: DatabaseId::from_bytes([0; 16]),
            incarnation: None,
        },
    )
    .unwrap()
}

fn legacy_owner(path: &Path, control: &StorageReadControl) -> Box<dyn Send + Sync> {
    let file = owner_file(path);
    let _admission = file.admit(control).unwrap();
    file.retain(0, control).unwrap()
}

#[test]
fn incompatible_owners_reject_all_record_mappings_before_initialization() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(1 << 24);
    for mode in 0..4 {
        for mapping in 0..3 {
            let path = directory.path().join(format!("owner-{mode}-{mapping}.db"));
            let primary = connection(&path, mode);
            let predecessor = legacy_owner(&path, &control);
            let initialize = || match mapping {
                0 => SQLiteRecordStore::new(&primary),
                1 => SQLiteRecordStore::for_native(&primary, &control),
                _ => SQLiteRecordStore::for_key_value(&primary, &control),
            };
            let error = initialize().err().expect("live predecessor is rejected");
            assert!(
                error.to_string().contains("incompatible database owners"),
                "{error}"
            );
            primary
                .with(|sqlite| {
                    let count: i64 = sqlite.query_row(
                        "SELECT count(*) FROM sqlite_schema WHERE name LIKE '_uqa_mvcc_%'",
                        [],
                        |row| row.get(0),
                    )?;
                    assert_eq!(count, 0, "failed admission must not initialize records");
                    Ok(())
                })
                .unwrap();
            drop(predecessor);
            let store = initialize().unwrap();
            let identity = store.database_id();
            // Independently opened current owners can attach without a cold epoch.
            let peer = connection(&path, mode);
            let peer_store = match mapping {
                0 => SQLiteRecordStore::new(&peer),
                1 => SQLiteRecordStore::for_native(&peer, &control),
                _ => SQLiteRecordStore::for_key_value(&peer, &control),
            }
            .unwrap();
            assert_eq!(peer_store.database_id(), identity);
        }
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn initial_catalog_restoration_retains_admission_until_transaction_completion() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("restore.db");
    let control = StorageReadControl::with_limit(1 << 24);
    let connection = connection(&path, 0);
    let predecessor = legacy_owner(&path, &control);
    assert!(matches!(
        connection.begin_native_initial_restore(),
        Err(SQLiteError::DatabaseCoordinationUpgradeBusy)
    ));
    assert!(!connection.in_transaction());
    drop(predecessor);
    for commit in [false, true] {
        connection.begin_native_initial_restore().unwrap();
        let catalog = crate::Catalog::open(connection.clone()).unwrap();
        catalog.set_metadata("protocol", "restored").unwrap();
        probe_admission(&path, true);
        let file = owner_file(&path);
        let cancelled = StorageReadControl::with_limit(1 << 20);
        cancelled.cancellation().cancel();
        assert!(file.admit(&cancelled).is_err());
        if commit {
            connection.commit_transaction().unwrap();
        } else {
            connection.rollback_transaction().unwrap();
        }
        // Completion must release the guard while the pool owner remains live.
        drop(file.admit(&control).unwrap());
        probe_admission(&path, false);
    }
}

fn probe_admission(path: &Path, held: bool) {
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "connection::ownership::tests::protocol::predecessor_owner_peer",
            "--test-threads=1",
        ])
        .env("UQA_PREDECESSOR_OWNER_PATH", path)
        .env(
            "UQA_PREDECESSOR_OWNER_PROBE",
            if held { "held" } else { "free" },
        )
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
}

struct Peer(Child);
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_predecessor_process_excludes_record_admission_until_death() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("peer.db");
    let connection = connection(&path, 0);
    let mut peer = Peer(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "connection::ownership::tests::protocol::predecessor_owner_peer",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("UQA_PREDECESSOR_OWNER_PATH", &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let output = peer.0.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(output).lines() {
            if line.unwrap().contains("UQA_PREDECESSOR_OWNER_READY") {
                send.send(()).unwrap();
                break;
            }
        }
    });
    receive
        .recv_timeout(std::time::Duration::from_secs(60))
        .unwrap();
    reader.join().unwrap();
    let error = SQLiteRecordStore::new(&connection).err().unwrap();
    assert!(
        error.to_string().contains("incompatible database owners"),
        "{error}"
    );
    peer.0.kill().unwrap();
    peer.0.wait().unwrap();
    SQLiteRecordStore::new(&connection).unwrap();
}

#[test]
#[ignore = "subprocess entry point for predecessor owner admission"]
fn predecessor_owner_peer() {
    let path = std::env::var_os("UQA_PREDECESSOR_OWNER_PATH").unwrap();
    if let Ok(expected) = std::env::var("UQA_PREDECESSOR_OWNER_PROBE") {
        let mut sidecar = path.clone();
        sidecar.push(".uqa-owners");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(sidecar)
            .unwrap();
        let result = uqa_storage::native_file::try_lock_byte(&file, 0, true);
        if expected == "held" {
            assert!(result.is_err_and(|error| uqa_storage::native_file::lock_would_block(&error)));
        } else {
            result.unwrap();
        }
        return;
    }
    let control = StorageReadControl::with_limit(1 << 20);
    let _owner = legacy_owner(Path::new(&path), &control);
    println!("UQA_PREDECESSOR_OWNER_READY");
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
}
