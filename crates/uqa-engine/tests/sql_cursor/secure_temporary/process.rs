//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A fresh child owns each temporary directory; test runners never mutate process-global temp settings.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use super::{assert_original_mutation, open, KEY, MODE_ENV, PATH_ENV, SECRET};

struct Process {
    child: Child,
    mode: usize,
    events: mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Process {
    fn start(path: &Path, temporary: &Path, mode: usize) -> Self {
        let test = concat!(
            module_path!(),
            "::retained_temporary_data_is_encrypted_and_released_across_provider_modes"
        );
        let test = test.replace("::process::", "::");
        let (_, test) = test.split_once("::").unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(PATH_ENV, path)
            .env(MODE_ENV, mode.to_string())
            .env("TMPDIR", temporary)
            .env("TMP", temporary)
            .env("TEMP", temporary)
            .env("SQLITE_TMPDIR", temporary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (sender, events) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            mode,
            events,
            reader: Some(reader),
        }
    }

    fn expect(&self, expected: &str) {
        loop {
            let line = self
                .events
                .recv_timeout(Duration::from_secs(60))
                .unwrap_or_else(|error| {
                    panic!(
                        "provider mode {} child {} did not reach {expected}: {error}",
                        self.mode,
                        self.child.id()
                    )
                });
            if let Some((_, event)) = line.split_once("retained-temp:") {
                assert_eq!(event, expected);
                return;
            }
        }
    }

    fn command(&mut self, command: &str) {
        let input = self.child.stdin.as_mut().unwrap();
        writeln!(input, "{command}").unwrap();
        input.flush().unwrap();
    }

    fn finish(&mut self, killed: bool) {
        if killed {
            self.child.kill().unwrap();
        }
        assert_eq!(self.child.wait().unwrap().success(), !killed);
        self.reader.take().unwrap().join().unwrap();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn files(directory: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            paths.extend(files(&entry.path()));
        } else {
            paths.push(entry.path());
        }
    }
    paths
}

fn protected_files(directory: &Path) -> Vec<PathBuf> {
    let paths = files(directory);
    assert!(
        !paths.is_empty(),
        "the fixture must retain actual temporary files"
    );
    for path in &paths {
        let bytes = std::fs::read(path).unwrap();
        assert!(
            !bytes
                .windows(SECRET.len())
                .any(|window| window == SECRET.as_bytes()),
            "plaintext in {}",
            path.display()
        );
        assert!(
            !bytes
                .windows(KEY.len())
                .any(|window| window == KEY.as_bytes()),
            "database credential in {}",
            path.display()
        );
        assert!(
            !bytes.starts_with(b"SQLite format 3\0"),
            "unkeyed temporary database {}",
            path.display()
        );
    }
    paths
}

pub(super) fn verify(mode: usize, kill: bool) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("database");
    let temporary = directory.path().join("temporary");
    std::fs::create_dir(&temporary).unwrap();
    let mut process = Process::start(&path, &temporary, mode);
    for (held, released) in [
        ("cursor-held", "cursor-released"),
        ("portal-held", "portal-released"),
    ] {
        process.expect(held);
        protected_files(&temporary);
        process.command("continue");
        process.expect(released);
        assert!(
            files(&temporary).is_empty(),
            "temporary files survived {released} in provider mode {mode}"
        );
        process.command("continue");
    }
    process.expect("mutation-held");
    let retained = protected_files(&temporary);
    assert!(
        retained.iter().any(|path| path
            .file_name()
            .is_some_and(|name| name == "overlay.sqlite")),
        "the actual INSERT conflict owner must be alive"
    );
    assert!(
        retained
            .iter()
            .any(|path| path.parent() == Some(temporary.as_path())),
        "the prepared mutation must retain its tuple spool beside the conflict index"
    );
    if kill {
        process.finish(true);
        protected_files(&temporary);
        let (engine, records) = open(&path, mode);
        assert_original_mutation(&engine);
        drop(engine);
        let control = uqa_storage::read_control::StorageReadControl::with_limit(1 << 20);
        records.reclaim_versions(&control).unwrap();
        assert_eq!(records.reclaim_versions(&control).unwrap(), 0);
        drop(records);
    } else {
        process.command("cancel");
        process.expect("mutation-released");
        assert!(
            files(&temporary).is_empty(),
            "cancelled mutation retained temporary files in provider mode {mode}"
        );
        process.command("continue");
        process.expect("closed");
        process.finish(false);
        assert!(files(&temporary).is_empty());
    }
}
