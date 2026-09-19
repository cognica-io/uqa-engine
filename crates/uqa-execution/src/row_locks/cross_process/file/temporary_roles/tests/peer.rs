//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{BufRead, Write};
use std::process::{Child, Stdio};

const PREFIX: &str = "UQA_TEMPORARY_ROLE_RESPONSE ";

pub(super) struct Peer {
    child: Child,
    responses: std::sync::mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Peer {
    pub(super) fn start(path: &std::path::Path) -> Self {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "row_locks::cross_process::file::temporary_roles::tests::peer::temporary_role_peer",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("UQA_TEMPORARY_ROLE_TEST_PATH", path)
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
                if let Some((_, response)) = line.split_once(PREFIX) {
                    if send.send(response.to_owned()).is_err() {
                        break;
                    }
                }
            }
        });
        let peer = Self {
            child,
            responses,
            reader: Some(reader),
        };
        assert_eq!(peer.response(), "ready");
        peer
    }

    fn response(&self) -> String {
        self.responses
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("temporary role peer response")
    }

    pub(super) fn request(&mut self, command: &str) -> String {
        let input = self.child.stdin.as_mut().unwrap();
        writeln!(input, "{command}").unwrap();
        input.flush().unwrap();
        self.response()
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
    println!("{PREFIX}{value}");
    std::io::stdout().flush().unwrap();
}

#[test]
#[ignore = "subprocess entry point for native temporary dependency tests"]
fn temporary_role_peer() {
    let path = std::env::var_os("UQA_TEMPORARY_ROLE_TEST_PATH").unwrap();
    let coordinator = FileLockCoordinator::open(std::path::Path::new(&path)).unwrap();
    let cancel = CancellationToken::new();
    respond("ready");
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let mut parts = line.split_whitespace();
        let command = parts.next().unwrap();
        let session = parts.next().unwrap().parse().unwrap();
        let role = parts.next().unwrap().parse().unwrap();
        match command {
            "admission" => {
                coordinator
                    .apply_byte_mode(ADMISSION_BYTE, None, Some(true))
                    .unwrap();
                respond("admitted");
            }
            "retain" => {
                coordinator
                    .retain_temporary_role(session, role, &cancel)
                    .unwrap();
                respond("retained");
            }
            "release" => {
                coordinator.release_temporary_role(session, role);
                respond("released");
            }
            "probe" => respond(
                coordinator
                    .foreign_temporary_role_reference(role, &cancel)
                    .unwrap(),
            ),
            _ => panic!("unexpected command {line}"),
        }
    }
}
