//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Kill only a helper that has announced the owned resource or durable boundary under test.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
};

use super::*;

const PATH: &str = "UQA_SQLITE_RESTORE_TEST_PATH";
const MODE: &str = "UQA_SQLITE_RESTORE_TEST_MODE";
const ACTION: &str = "UQA_SQLITE_RESTORE_TEST_ACTION";
const SOURCE: &str = "UQA_SQLITE_RESTORE_TEST_SOURCE";
const TARGET: &str = "UQA_SQLITE_RESTORE_TEST_TARGET";

struct ReadyPeer {
    child: Child,
    ready: bool,
}

impl ReadyPeer {
    fn start(path: &Path, mode: usize, action: &str, request: DatabaseRestore) -> Self {
        let name = concat!(module_path!(), "::restore_process_helper");
        let (_, test) = name.split_once("::").unwrap();
        let mut peer = Self {
            child: Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test, "--nocapture"])
                .env(PATH, path)
                .env(MODE, mode.to_string())
                .env(ACTION, action)
                .env(
                    SOURCE,
                    u128::from_be_bytes(request.source().as_bytes()).to_string(),
                )
                .env(
                    TARGET,
                    u128::from_be_bytes(request.target().as_bytes()).to_string(),
                )
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
            ready: false,
        };
        let mut output = BufReader::new(peer.child.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            assert_ne!(
                output.read_line(&mut line).unwrap(),
                0,
                "restore peer exited before readiness: {:?}",
                peer.child.wait()
            );
            if line.trim() == "restore-ready" {
                peer.ready = true;
                return peer;
            }
        }
    }

    fn crash(mut self) {
        self.child.kill().unwrap();
        assert!(!self.child.wait().unwrap().success());
    }
}

impl Drop for ReadyPeer {
    fn drop(&mut self) {
        if self.ready && self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn ready() {
    println!("restore-ready");
    std::io::stdout().flush().unwrap();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    panic!("owned crash helper unexpectedly resumed");
}

#[test]
fn restore_process_helper() {
    let Some(path) = std::env::var_os(PATH) else {
        return;
    };
    let path = Path::new(&path);
    let mode = std::env::var(MODE).unwrap().parse().unwrap();
    let identity = |name| {
        DatabaseId::from_bytes(
            std::env::var(name)
                .unwrap()
                .parse::<u128>()
                .unwrap()
                .to_be_bytes(),
        )
    };
    let request = DatabaseRestore::from_identities(identity(SOURCE), identity(TARGET)).unwrap();
    let action = std::env::var(ACTION).unwrap();
    let control = control();
    if action == "resource" {
        use uqa_storage::mvcc::{ResourceLeaseId, ResourceLeaseProvider, ResourceLeaseRequest};
        let connection = open(path, mode).unwrap();
        let store = records(&connection, false);
        let lease = store
            .try_acquire_resource(
                ResourceLeaseId::new(request.source(), 7777).unwrap(),
                ResourceLeaseRequest::Claim,
                &control,
            )
            .unwrap()
            .unwrap();
        drop((store, connection));
        ready();
        drop(lease);
    } else if action == "owner" {
        let connection = open(path, mode).unwrap();
        let store = records(&connection, false);
        let (participant, snapshot) = store
            .admit_serializable(false, &control, || store.snapshot(&control))
            .unwrap();
        // Retain only detached resources after their original adapters have closed.
        drop(store);
        drop(connection);
        ready();
        drop((participant, snapshot));
    } else {
        let at = match action.as_str() {
            "intent" => Boundary::IntentPublished,
            "coordinator" => Boundary::CoordinatorPublished,
            _ => panic!("unknown restore crash boundary"),
        };
        let _injection = inject(at, || {
            ready();
            Ok(())
        });
        restored(path, mode, request, &control).unwrap();
    }
}

#[test]
fn process_death_releases_detached_owners_before_restoration() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("owner.db");
        let backup = seed(&path, mode, false);
        let peer = ReadyPeer::start(&path, mode, "owner", backup.request);
        assert!(matches!(
            restored(&path, mode, backup.request, &control()),
            Err(SQLiteError::DatabaseRestoreBusy)
        ));
        peer.crash();
        let connection = restored(&path, mode, backup.request, &control()).unwrap();
        verify(&connection, &backup, false);
    }
}

#[test]
fn process_death_releases_detached_resource_owners_before_restoration() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("resource-owner.db");
        let backup = seed(&path, mode, false);
        let peer = ReadyPeer::start(&path, mode, "resource", backup.request);
        assert!(matches!(
            restored(&path, mode, backup.request, &control()),
            Err(SQLiteError::DatabaseRestoreBusy)
        ));
        peer.crash();
        let connection = restored(&path, mode, backup.request, &control()).unwrap();
        verify(&connection, &backup, false);
    }
}

#[test]
fn process_crashes_at_each_durable_boundary_resume_both_sqlite_mappings() {
    for mode in 0..4 {
        for native in [false, true] {
            for action in ["intent", "coordinator"] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("crash.db");
                let backup = seed(&path, mode, native);
                ReadyPeer::start(&path, mode, action, backup.request).crash();
                assert!(matches!(
                    open(&path, mode),
                    Err(SQLiteError::DatabaseRestoreIncomplete)
                ));
                let connection = restored(&path, mode, backup.request, &control()).unwrap();
                verify(&connection, &backup, native);
            }
        }
    }
}
