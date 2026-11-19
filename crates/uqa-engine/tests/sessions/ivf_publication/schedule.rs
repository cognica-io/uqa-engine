//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound a child suite by completed fixtures, retaining its single-process history.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

struct Progress<'a> {
    prefix: &'a str,
    expected: usize,
    completed: BTreeSet<String>,
    summary: bool,
    allowance: Duration,
    deadline: Instant,
}

impl<'a> Progress<'a> {
    fn new(prefix: &'a str, expected: usize, allowance: Duration, now: Instant) -> Self {
        Self {
            prefix,
            expected,
            completed: BTreeSet::new(),
            summary: false,
            allowance,
            deadline: now + allowance,
        }
    }

    fn observe(&mut self, line: &str, now: Instant) -> Result<(), String> {
        if let Some((name, outcome)) = line
            .strip_prefix("test ")
            .and_then(|rest| rest.rsplit_once(" ... "))
        {
            if !name.starts_with(self.prefix) || outcome != "ok" {
                return Err(format!("unexpected fixture result: {line}"));
            }
            if !self.completed.insert(name.to_owned()) || self.completed.len() > self.expected {
                return Err(format!("duplicate or extra fixture result: {line}"));
            }
            self.deadline = now + self.allowance;
        } else if line.starts_with("test result:") {
            let expected = format!(
                "test result: ok. {} passed; 0 failed; 0 ignored;",
                self.expected
            );
            if self.summary || self.completed.len() != self.expected || !line.starts_with(&expected)
            {
                return Err(format!("unexpected fixture summary: {line}"));
            }
            self.summary = true;
        }
        Ok(())
    }

    fn remaining(&self, now: Instant) -> Result<Duration, String> {
        self.deadline
            .checked_duration_since(now)
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| self.stalled())
    }

    fn stalled(&self) -> String {
        format!(
            "no new fixture completed within {:?}; completed {}/{}",
            self.allowance,
            self.completed.len(),
            self.expected
        )
    }

    fn observe_line(&mut self, bytes: &[u8], label: &str) -> Result<(), String> {
        let raw = std::str::from_utf8(bytes)
            .map_err(|error| format!("cannot read child suite: {error}"))?;
        let line = raw
            .strip_suffix('\n')
            .map_or(raw, |line| line.strip_suffix('\r').unwrap_or(line));
        eprintln!("{label}: {line}");
        self.observe(line, Instant::now())
    }

    fn finish(self) -> Result<(), String> {
        if self.summary && self.completed.len() == self.expected {
            Ok(())
        } else {
            Err(format!(
                "incomplete child suite: {}/{} fixtures, summary={}",
                self.completed.len(),
                self.expected,
                self.summary
            ))
        }
    }
}

struct OwnedChild {
    process: Option<Child>,
    output: BufReader<File>,
}

impl OwnedChild {
    fn spawn(command: &mut Command, capture: tempfile::NamedTempFile) -> io::Result<Self> {
        let output = BufReader::new(capture.reopen()?);
        let process = command
            .stdout(capture.into_file())
            .stderr(Stdio::inherit())
            .spawn();
        command.stdout(Stdio::inherit());
        Ok(Self {
            process: Some(process?),
            output,
        })
    }

    fn status(&mut self) -> Result<Option<ExitStatus>, String> {
        self.process
            .as_mut()
            .unwrap()
            .try_wait()
            .map_err(|error| format!("cannot reap child suite: {error}"))
    }

    fn watch(&mut self, mut progress: Progress<'_>, label: &str) -> Result<(), String> {
        let mut offset = 0;
        let mut line = Vec::new();
        loop {
            let deadline = progress.deadline;
            let due = Instant::now() >= deadline;
            let status = self.status()?;
            // At direct-child exit, every byte it wrote is visible. A descendant can still hold stdout.
            let end = self
                .output
                .get_ref()
                .metadata()
                .map_err(|error| format!("cannot read child suite: {error}"))?
                .len();
            let available = end
                .checked_sub(offset)
                .ok_or("child suite output was truncated")?;
            // Once due, inspect the entire finite backlog before declaring a live child stalled.
            let limit = if due || status.is_some() {
                available
            } else {
                available.min(64 * 1024)
            };
            let mut snapshot = self.output.by_ref().take(limit);
            while snapshot.limit() != 0 {
                let read = snapshot
                    .read_until(b'\n', &mut line)
                    .map_err(|error| format!("cannot read child suite: {error}"))?;
                if read == 0 {
                    return Err("child suite output was truncated".to_owned());
                }
                if line.ends_with(b"\n") {
                    progress.observe_line(&line, label)?;
                    line.clear();
                }
            }
            offset += limit;
            if let Some(status) = status {
                self.process.take();
                if !line.is_empty() {
                    progress.observe_line(&line, label)?;
                }
                eprintln!("{label}: child suite {status}");
                if !status.success() {
                    return Err(format!("child suite failed: {status}"));
                }
                return progress.finish();
            }
            match progress.remaining(Instant::now()) {
                Ok(remaining) if offset == end => {
                    std::thread::sleep(remaining.min(Duration::from_millis(10)));
                }
                Err(error) if due && deadline == progress.deadline && self.status()?.is_none() => {
                    return Err(if progress.summary {
                        format!("{error}; successful summary observed, child still running")
                    } else {
                        error
                    });
                }
                _ => (),
            }
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub(super) fn run(
    command: &mut Command,
    prefix: &str,
    expected: usize,
    allowance: Duration,
    label: &str,
) -> Result<(), String> {
    tempfile::NamedTempFile::new()
        .and_then(|capture| OwnedChild::spawn(command, capture))
        .map_err(|error| format!("{label}: cannot start child suite: {error}"))?
        .watch(
            Progress::new(prefix, expected, allowance, Instant::now()),
            label,
        )
        .map_err(|error| format!("{label}: {error}"))
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod tests;
