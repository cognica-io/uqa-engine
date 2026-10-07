//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Causally ordered subprocess requests for the native relation-lock owner tests.

use std::fmt::Write as _;
use std::io::{BufRead, Write};
use std::process::{Child, Stdio};

use super::*;

const RESPONSE_PREFIX: &str = "UQA_RELATION_RESPONSE ";

pub(super) struct Peer {
    child: Child,
    responses: std::sync::mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Peer {
    pub(super) fn start(path: &std::path::Path) -> Self {
        Self::start_for_relation(path, RELATION)
    }

    pub(super) fn start_for_relation(path: &std::path::Path, relation: &[u8]) -> Self {
        Self::start_with_key(path, relation, None)
    }

    pub(super) fn start_encrypted(path: &std::path::Path, relation: &[u8], key: &str) -> Self {
        Self::start_with_key(path, relation, Some(key))
    }

    fn start_with_key(path: &std::path::Path, relation: &[u8], key: Option<&str>) -> Self {
        let mut encoded = String::with_capacity(relation.len() * 2);
        for byte in relation {
            write!(&mut encoded, "{byte:02x}").unwrap();
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "row_locks::cross_process::file::relations::tests::peer::relation_lock_peer",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("UQA_RELATION_LOCK_TEST_PATH", path)
            .env("UQA_RELATION_LOCK_TEST_IDENTITY", encoded)
            .env("UQA_RELATION_LOCK_TEST_KEY", key.unwrap_or_default())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, responses) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some((_, response)) = line.split_once(RESPONSE_PREFIX) {
                    if send.send(response.to_owned()).is_err() {
                        break;
                    }
                }
            }
        });
        let mut peer = Self {
            child,
            responses,
            reader: Some(reader),
        };
        assert_eq!(peer.response(), "ready");
        peer
    }

    pub(super) fn request(&mut self, request: &str) -> String {
        let input = self.child.stdin.as_mut().unwrap();
        writeln!(input, "{request}").unwrap();
        input.flush().unwrap();
        self.response()
    }

    fn response(&mut self) -> String {
        self.responses
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap_or_else(|error| {
                panic!(
                    "relation-lock peer response failed: {error}; status {:?}",
                    self.child.try_wait()
                )
            })
    }

    pub(super) fn terminate(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
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

fn respond(value: impl std::fmt::Display) {
    println!("{RESPONSE_PREFIX}{value}");
    std::io::stdout().flush().unwrap();
}

#[test]
#[ignore = "subprocess entry point for native relation-lock owner tests"]
fn relation_lock_peer() {
    let path = std::env::var_os("UQA_RELATION_LOCK_TEST_PATH").unwrap();
    let key = std::env::var("UQA_RELATION_LOCK_TEST_KEY")
        .ok()
        .filter(|key| !key.is_empty())
        .map(|key| uqa_storage::StorageEncryptionKey::new(&key));
    let opened = FileLockCoordinator::open_with_key(std::path::Path::new(&path), key);
    let encoded = std::env::var("UQA_RELATION_LOCK_TEST_IDENTITY").unwrap();
    let relation = (0..encoded.len())
        .step_by(2)
        .map(|offset| u8::from_str_radix(&encoded[offset..offset + 2], 16).unwrap())
        .collect::<Vec<_>>();
    respond("ready");
    let mut waiting = None;
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let coordinator = match &opened {
            Ok(coordinator) => coordinator,
            Err(error) => {
                respond(format!("error {error}"));
                continue;
            }
        };
        let mut command = line.split_whitespace();
        let operation = command.next().unwrap();
        let mode = command
            .next()
            .map(|mode| RelationLockMode::ALL[mode.parse::<usize>().unwrap()]);
        let session = command
            .next()
            .map_or(PEER_SESSION, |session| session.parse().unwrap());
        match operation {
            "try" => match coordinator.try_relation_claim(session, &relation, mode.unwrap()) {
                Ok(Ok(())) => respond("granted"),
                Ok(Err(RelationClaimWait::AdmissionBusy)) => respond("admission busy"),
                Ok(Err(RelationClaimWait::Conflict(claim))) => {
                    respond(format!("conflict {}", claim.offset));
                }
                Err(error) => respond(format!("error {error}")),
            },
            "release" => {
                coordinator.release_relation(session, &relation, mode.unwrap());
                respond("released");
            }
            "wait" => {
                coordinator.clear_wait(session);
                waiting = Some(
                    coordinator
                        .pin_relation(&relation, &uqa_core::CancellationToken::new())
                        .unwrap(),
                );
                coordinator.register_wait(
                    session,
                    relation_wait_claim(waiting.as_ref().unwrap().slot(), mode.unwrap()),
                );
                respond("waiting");
            }
            "clear" => {
                coordinator.clear_wait(session);
                waiting.take();
                respond("cleared");
            }
            "admission" => {
                coordinator
                    .apply_byte_mode(RELATION_ADMISSION_BYTE, None, Some(true))
                    .unwrap();
                respond("admitted");
            }
            _ => panic!("unexpected relation-lock peer command {line}"),
        }
    }
}
