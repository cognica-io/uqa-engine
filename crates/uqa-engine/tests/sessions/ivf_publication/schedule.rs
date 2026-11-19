//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound a child suite by completed fixtures, retaining its single-process history.

use std::collections::BTreeSet;
use std::io::BufRead;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
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
        } else {
            self.remaining(now)?;
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

    fn receive(
        &self,
        receiver: &Receiver<std::io::Result<String>>,
    ) -> Result<Option<String>, String> {
        let result = match receiver.try_recv() {
            Ok(line) => Ok(line),
            Err(TryRecvError::Disconnected) => Err(RecvTimeoutError::Disconnected),
            Err(TryRecvError::Empty) => receiver.recv_timeout(self.remaining(Instant::now())?),
        };
        match result {
            Ok(Ok(line)) => Ok(Some(line)),
            Ok(Err(error)) => Err(format!("cannot read child suite: {error}")),
            Err(RecvTimeoutError::Timeout) => Err(self.stalled()),
            Err(RecvTimeoutError::Disconnected) => Ok(None),
        }
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

struct OwnedChild(Option<Child>);

impl OwnedChild {
    fn finish(&mut self, progress: &Progress<'_>) -> Result<ExitStatus, String> {
        loop {
            match self.0.as_mut().unwrap().try_wait() {
                Ok(Some(status)) => {
                    self.0.take();
                    return Ok(status);
                }
                Ok(None) => std::thread::sleep(
                    progress
                        .remaining(Instant::now())?
                        .min(Duration::from_millis(10)),
                ),
                Err(error) => return Err(format!("cannot reap child suite: {error}")),
            }
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
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
    std::thread::scope(|scope| {
        let mut child = OwnedChild(Some(
            command
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|error| format!("cannot start child suite: {error}"))?,
        ));
        let stdout = child.0.as_mut().unwrap().stdout.take().unwrap();
        let (send, receive) = mpsc::sync_channel(64);
        scope.spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        let mut progress = Progress::new(prefix, expected, allowance, Instant::now());
        while let Some(line) = progress.receive(&receive)? {
            eprintln!("{label}: {line}");
            progress.observe(&line, Instant::now())?;
        }
        drop(receive);
        let status = child.finish(&progress)?;
        if !status.success() {
            return Err(format!("child suite failed: {status}"));
        }
        progress.finish()
    })
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod tests;
