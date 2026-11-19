//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{io, sync::Arc};

use parking_lot::Mutex;

/// Shared physical byte allowance, including ciphertext slots and framing. A
/// reservation follows the file owner, including reopened handles and failed
/// truncation; filesystem allocation metadata is outside this byte count.
#[derive(Clone, Debug)]
pub(crate) struct FileBudget(Arc<Mutex<Usage>>);

#[derive(Debug)]
struct Usage {
    limit: u64,
    used: u64,
    peak: u64,
}

#[derive(Debug, thiserror::Error)]
#[error("temporary files require {required} bytes, exceeding limit {limit}")]
pub(crate) struct FileLimit {
    pub required: u64,
    pub limit: u64,
}

pub(super) struct Reservation {
    budget: FileBudget,
    bytes: u64,
}

impl FileBudget {
    pub(crate) fn new(limit: u64) -> Self {
        Self(Arc::new(Mutex::new(Usage {
            limit,
            used: 0,
            peak: 0,
        })))
    }

    pub(crate) fn shares_allowance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    #[cfg(any(test, windows, all(unix, not(target_os = "emscripten"))))]
    pub(crate) fn limit(&self) -> u64 {
        self.0.lock().limit
    }

    pub(super) fn reserve(&self) -> Reservation {
        Reservation {
            budget: self.clone(),
            bytes: 0,
        }
    }

    #[cfg(any(test, windows, all(unix, not(target_os = "emscripten"))))]
    pub(crate) fn usage(&self) -> (u64, u64) {
        let usage = self.0.lock();
        (usage.used, usage.peak)
    }
}

impl Reservation {
    pub(super) fn budget(&self) -> FileBudget {
        self.budget.clone()
    }

    pub(super) fn resize(&mut self, bytes: u64) -> io::Result<()> {
        let mut usage = self.budget.0.lock();
        let required = (usage.used - self.bytes)
            .checked_add(bytes)
            .ok_or_else(|| io::Error::other("temporary file allowance overflow"))?;
        if required > usage.limit {
            return Err(io::Error::other(FileLimit {
                required,
                limit: usage.limit,
            }));
        }
        usage.used = required;
        usage.peak = usage.peak.max(required);
        self.bytes = bytes;
        Ok(())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.0.lock().used -= self.bytes;
    }
}
