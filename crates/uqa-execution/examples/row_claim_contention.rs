//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent-process claim-table workload; timing output is diagnostic until qualified.

use std::io::{BufRead, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uqa_core::CancellationToken;
use uqa_execution::row_locks::{
    shared_provider_manager, LockAcquire, LockRequest, RowLockKey, RowLockManager,
};
use uqa_sql::ast::{LockStrength, LockWait};
use uqa_storage::{
    PersistentStorageIdentity, PersistentStorageProvider, PersistentStorageSession,
    StorageBackendError, StorageBackendResult,
};

// The workload exercises the execution owner's real coordinator, without opening a database.
struct IdentityProvider;

impl PersistentStorageProvider for IdentityProvider {
    fn open_session(&self) -> StorageBackendResult<PersistentStorageSession> {
        Err(StorageBackendError::Other(
            "the claim workload does not open storage sessions".into(),
        ))
    }
}

fn manager(path: &Path) -> Arc<RowLockManager> {
    let provider: Arc<dyn PersistentStorageProvider> = Arc::new(IdentityProvider);
    let manager = shared_provider_manager(
        Some(PersistentStorageIdentity::File(path.to_owned())),
        &provider,
    );
    assert!(manager.has_cross_process_coordination());
    manager
}

fn digest(process: usize, key: usize) -> [u8; 32] {
    let mut bytes = [0; 32];
    bytes[..8].copy_from_slice(&(process as u64).to_be_bytes());
    bytes[24..].copy_from_slice(&(key as u64).to_be_bytes());
    bytes
}

fn request(table: u64, cancel: &CancellationToken) -> LockRequest<'_> {
    LockRequest {
        session_id: 1,
        key: RowLockKey { table, doc_id: 0 },
        strength: LockStrength::ForUpdate,
        mark: 1,
        wait: LockWait::NoWait,
        cancel,
        relation: "claim workload",
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Response {
    state: String,
    keys: usize,
    nanoseconds: Option<u128>,
}

fn respond(state: &str, keys: usize, nanoseconds: Option<u128>) {
    println!(
        "{}",
        serde_json::to_string(&Response {
            state: state.to_owned(),
            keys,
            nanoseconds,
        })
        .unwrap()
    );
    std::io::stdout().flush().unwrap();
}

fn worker(path: &Path, process: usize, keys: usize, measure: bool) {
    assert!(process < 8 && [64, 512].contains(&keys));
    let manager = manager(path);
    let cancel = CancellationToken::new();
    let mut commands = std::io::stdin().lock().lines();
    respond("ready", keys, None);
    while let Some(command) = commands.next() {
        let command = command.unwrap();
        if command == "finish" {
            return;
        }
        assert_eq!(command, "acquire");
        let identities = (0..keys)
            .map(|key| manager.retain_key_reservation(digest(process, key)))
            .collect::<Vec<_>>();
        let requests = identities
            .iter()
            .map(|identity| request(identity.table_key(), &cancel))
            .collect::<Vec<_>>();
        let started = measure.then(Instant::now);
        let grants = manager.acquire_batch(&requests).unwrap();
        let elapsed = started.map(|start| start.elapsed().as_nanos());
        assert_eq!(grants.len(), keys);
        assert!(grants.iter().all(|grant| matches!(
            grant,
            LockAcquire::Granted {
                acquisition: Some(_),
                waited: false,
                foreign_waited: false,
            }
        )));
        respond("held", keys, elapsed);
        assert_eq!(commands.next().unwrap().unwrap(), "release");
        let started = measure.then(Instant::now);
        manager.release_session(1);
        let elapsed = started.map(|start| start.elapsed().as_nanos());
        drop(identities);
        respond("released", keys, elapsed);
    }
}

struct Peer {
    child: Child,
    responses: mpsc::Receiver<Response>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Peer {
    fn new(path: &Path, process: usize, keys: usize, measure: bool) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--worker")
            .arg(path)
            .args([process.to_string(), keys.to_string(), measure.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, responses) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let response = serde_json::from_str(&line.unwrap()).unwrap();
                if sender.send(response).is_err() {
                    return;
                }
            }
        });
        let mut peer = Self {
            child,
            responses,
            reader: Some(reader),
        };
        assert_eq!(peer.response("ready", keys).nanoseconds, None);
        peer
    }

    fn send(&mut self, command: &str) {
        let input = self.child.stdin.as_mut().unwrap();
        writeln!(input, "{command}").unwrap();
        input.flush().unwrap();
    }

    fn response(&mut self, state: &str, keys: usize) -> Response {
        let response = self
            .responses
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|error| {
                panic!(
                    "claim peer failed: {error}; status {:?}",
                    self.child.try_wait()
                )
            });
        assert_eq!(response.state, state);
        assert_eq!(response.keys, keys);
        response
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

fn verify_ownership(manager: &RowLockManager, processes: usize, keys: usize, held: bool) {
    let cancel = CancellationToken::new();
    for process in 0..processes {
        for key in [0, keys - 1] {
            let identity = manager.retain_key_reservation(digest(process, key));
            let result = manager.acquire(&request(identity.table_key(), &cancel));
            if held {
                assert_eq!(result.unwrap_err().sqlstate(), Some("55P03"));
            } else {
                assert!(matches!(result.unwrap(), LockAcquire::Granted { .. }));
                manager.release_session(1);
            }
        }
    }
}

fn workload(processes: usize, keys: usize, measure: bool) -> serde_json::Value {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("claims.db");
    let manager = manager(&path);
    let mut peers = (0..processes)
        .map(|process| Peer::new(&path, process, keys, measure))
        .collect::<Vec<_>>();
    let warmup = if measure { 8 } else { 0 };
    let rounds = if measure { 128 } else { 2 };
    let mut samples = Vec::new();
    for round in 0..warmup + rounds {
        let acquisition_started = measure.then(Instant::now);
        for peer in &mut peers {
            peer.send("acquire");
        }
        let acquired = peers
            .iter_mut()
            .map(|peer| peer.response("held", keys).nanoseconds)
            .collect::<Vec<_>>();
        let acquisition_wall_ns = acquisition_started.map(|start| start.elapsed().as_nanos());
        verify_ownership(&manager, processes, keys, true);
        let release_started = measure.then(Instant::now);
        for peer in &mut peers {
            peer.send("release");
        }
        let released = peers
            .iter_mut()
            .map(|peer| peer.response("released", keys).nanoseconds)
            .collect::<Vec<_>>();
        let release_wall_ns = release_started.map(|start| start.elapsed().as_nanos());
        if !measure {
            assert!(acquired.iter().chain(&released).all(Option::is_none));
        }
        verify_ownership(&manager, processes, keys, false);
        if round >= warmup && measure {
            assert!(acquired.iter().chain(&released).all(Option::is_some));
            samples.push(
                serde_json::json!({"acquire_ns": acquired, "release_ns": released,
                "acquisition_wall_ns": acquisition_wall_ns, "release_wall_ns": release_wall_ns}),
            );
        }
    }
    for peer in &mut peers {
        peer.send("finish");
        assert!(peer.child.wait().unwrap().success());
    }
    serde_json::json!({"processes": processes, "keys_per_process": keys,
        "warmup_rounds": warmup, "rounds": rounds, "correctness": true, "samples": samples})
}

fn main() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "--worker") && args.len() == 5 {
        worker(
            Path::new(&args[1]),
            args[2].to_str().unwrap().parse().unwrap(),
            args[3].to_str().unwrap().parse().unwrap(),
            args[4].to_str().unwrap().parse().unwrap(),
        );
        return;
    }
    assert!(
        args.len() == 1 && (args[0] == "--verify" || args[0] == "--measure"),
        "usage: row_claim_contention --verify | --measure"
    );
    let measure = args[0] == "--measure";
    assert!(
        !measure || !cfg!(debug_assertions),
        "measurement requires a release-profile executable"
    );
    let mut cases = Vec::new();
    for processes in [1, 2, 4, 8] {
        for keys in [64, 512] {
            cases.push(workload(processes, keys, measure));
        }
    }
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({"schema_version": 1,
            "timing_acceptance": false, "acceptance_status": "unqualified",
            "measurement": measure, "cases": cases}))
        .unwrap()
    );
}
