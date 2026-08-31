//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native sidecar coordinator and its durable state.

use std::collections::HashMap;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use parking_lot::Mutex;
use uqa_sql::ast::LockStrength;

use super::super::lock_strengths_conflict;
use super::{ByteClaim, PhysicalRowChangeTarget, RowChangeTarget};
use super::{PublishedRowChange, PublishedRowChangeKind, PublishedRowIdentity};

const CHANGE_JOURNAL_LOCK_BYTE: u64 = 10;
const SLOT_METADATA_LOCK_BYTE: u64 = 11;
#[cfg(windows)]
const MODE_TRANSITION_LOCK_BYTE: u64 = 12;
const TRANSACTION_XID_LOCK_BYTE: u64 = 13;
const TRANSACTION_XID_STATE_OFFSET: u64 = 16;
const TRANSACTION_XID_STATE_SIZE: usize = 16;
const TRANSACTION_XID_STATE_MAGIC: u32 = 0x5551_5849;
const TRANSACTION_XID_STATE_VERSION: u32 = 1;
const WAIT_SLOT_BASE: u64 = 64;
const WAIT_SLOT_SIZE: u64 = 32;
const WAIT_SLOT_COUNT: u64 = 256;
const HOLDER_SLOT_BASE: u64 = WAIT_SLOT_BASE + WAIT_SLOT_SIZE * WAIT_SLOT_COUNT;
const HOLDER_SLOT_SIZE: u64 = 32;
const HOLDER_SLOT_COUNT: u64 = 8192;
const CHANGE_ENTRY_SIZE: u64 = 48;
const CHANGE_ENTRY_MAGIC: u32 = 0x5551_4348;
const CHANGE_JOURNAL_WAIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Default)]
struct ByteClaimCounts {
    shared: u64,
    exclusive: u64,
}

impl ByteClaimCounts {
    fn mode(&self) -> Option<bool> {
        if self.exclusive > 0 {
            Some(true)
        } else if self.shared > 0 {
            Some(false)
        } else {
            None
        }
    }
}

struct CoordinatorState {
    claims: HashMap<u64, ByteClaimCounts>,
    /// Sessions of this process holding each claimed byte, so a cross-process wait-for walk can attribute a locally held byte to the session that owns it and follow that session's own wait.
    holders: HashMap<u64, Vec<u64>>,
    /// Sidecar wait slot advertised for each locally waiting session.
    wait_slots: HashMap<u64, u64>,
    /// Sidecar holder slots for each acquisition owned by a local session. The vector preserves duplicate acquisitions of the same byte.
    holder_slots: HashMap<(u64, u64, bool), Vec<u64>>,
    /// Holder-slot indexes owned by this process. Slot probing is on every durable row-lock acquisition, so deriving this set by scanning every acquisition makes a bulk write quadratic in the number of rows held by its transaction.
    occupied_holder_slots: Vec<bool>,
    /// Next holder slot to probe. Advancing past each allocation avoids restarting every acquisition at an unrelated hash location and repeatedly reading slots already known to be occupied by this process.
    next_holder_slot: u64,
}

/// Process-wide coordinator for one durable database. All engine sessions of this process share one descriptor while the in-process lock table arbitrates between local sessions. On POSIX, nothing else in the process may open the sidecar path because closing another descriptor to it would drop this process's record locks.
pub(in crate::row_locks) struct FileLockCoordinator {
    file: std::fs::File,
    change_file: std::fs::File,
    change_journal: Mutex<()>,
    transaction_xids: Mutex<()>,
    state: Mutex<CoordinatorState>,
}

mod claims;
mod journal;
mod platform;
mod waits;
mod xids;

use platform::{lock_would_block, process_alive, read_exact_at, write_all_at};

impl FileLockCoordinator {
    pub(in crate::row_locks) fn open(database_path: &Path) -> Result<Self, String> {
        let mut sidecar = database_path.as_os_str().to_owned();
        sidecar.push(".uqa-locks");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&sidecar)
            .map_err(|error| {
                format!(
                    "open cross-process lock file `{}`: {error}",
                    Path::new(&sidecar).display()
                )
            })?;
        let mut change_sidecar = database_path.as_os_str().to_owned();
        change_sidecar.push(".uqa-row-changes");
        let change_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&change_sidecar)
            .map_err(|error| {
                format!(
                    "open cross-process row-change journal `{}`: {error}",
                    Path::new(&change_sidecar).display()
                )
            })?;
        let pid = std::process::id();
        let coordinator = Self {
            file,
            change_file,
            change_journal: Mutex::new(()),
            transaction_xids: Mutex::new(()),
            state: Mutex::new(CoordinatorState {
                claims: HashMap::new(),
                holders: HashMap::new(),
                wait_slots: HashMap::new(),
                holder_slots: HashMap::new(),
                occupied_holder_slots: vec![false; HOLDER_SLOT_COUNT as usize],
                next_holder_slot: u64::from(pid).wrapping_mul(31) % HOLDER_SLOT_COUNT,
            }),
        };
        Ok(coordinator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holder_slot_cursor_tracks_bulk_claims_and_releases() {
        let directory = tempfile::tempdir().unwrap();
        let coordinator = FileLockCoordinator::open(&directory.path().join("bulk.db")).unwrap();
        let claims = (0..128)
            .map(|ordinal| ByteClaim {
                offset: 10_000 + ordinal,
                write: true,
            })
            .collect::<Vec<_>>();
        let session = 17;
        let mut state = coordinator.state.lock();
        let first_slot = state.next_holder_slot;

        for claim in &claims {
            coordinator.register_holder_slot(&mut state, session, *claim);
        }
        assert_eq!(
            state
                .occupied_holder_slots
                .iter()
                .filter(|occupied| **occupied)
                .count(),
            claims.len()
        );
        for (ordinal, claim) in claims.iter().enumerate() {
            let expected = (first_slot + ordinal as u64) % HOLDER_SLOT_COUNT;
            assert_eq!(
                state
                    .holder_slots
                    .get(&(session, claim.offset, claim.write))
                    .unwrap(),
                &[expected]
            );
        }
        assert_eq!(
            state.next_holder_slot,
            (first_slot + claims.len() as u64) % HOLDER_SLOT_COUNT
        );

        for claim in &claims {
            coordinator.clear_holder_slot(&mut state, session, *claim);
        }
        assert!(state.holder_slots.is_empty());
        assert!(state.occupied_holder_slots.iter().all(|occupied| !occupied));
    }
}
