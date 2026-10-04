//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native sidecar coordinator and its durable state.

use std::collections::HashMap;
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
const TRANSACTION_XID_ATTACHMENT_BYTE: u64 = 8;
const TRANSACTION_XID_LOCK_BYTE: u64 = 13;
const TRANSACTION_XID_STATE_OFFSET: u64 = 16;
const TRANSACTION_XID_STATE_SIZE: usize = 16;
const TRANSACTION_XID_STATE_MAGIC: u32 = 0x5551_5849;
const TRANSACTION_XID_STATE_VERSION: u32 = 1;
const TRANSACTION_XID_CURSOR_OFFSET: u64 = 32;
const TRANSACTION_XID_CURSOR_SIZE: usize = 24;
const TRANSACTION_XID_CURSOR_MAGIC: u32 = 0x5551_5843;
const TRANSACTION_XID_CURSOR_VERSION: u32 = 1;
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
    /// Acquisitions of local sessions whose holder slots are not yet published, counted per session, byte and mode. Only the cross-process wait-for walk reads holder slots, and a deadlock cycle closes only when its last member starts waiting, so publishing every pending holder before a local session advertises a wait keeps detection complete while claims stay free of slot I/O.
    pending_holders: HashMap<(u64, u64, bool), Vec<u64>>,
    /// Pending acquisitions by acquisition sequence, so publication assigns slots in acquisition order.
    pending_order: std::collections::BTreeMap<u64, (u64, ByteClaim)>,
    next_pending: u64,
    /// Holder-slot indexes owned by this process. Slot probing is on every durable row-lock acquisition, so deriving this set by scanning every acquisition makes a bulk write quadratic in the number of rows held by its transaction.
    occupied_holder_slots: Vec<bool>,
    /// Released slots are considered before the advancing probe cursor. Keeping them separate avoids rescanning live holders after each short-lived acquisition inside a larger transaction.
    released_holder_slots: Vec<u64>,
    /// Next holder slot to probe. Advancing past each allocation avoids restarting every acquisition at an unrelated hash location and repeatedly reading slots already known to be occupied by this process.
    next_holder_slot: u64,
    /// Row claims of local sessions, which live in the shared claim table instead of record locks and holder slots.
    rows: row_claims::RowClaims,
}

/// Process-wide coordinator for one durable database. All engine sessions of this process share one descriptor while the in-process lock table arbitrates between local sessions. On POSIX, nothing else in the process may open the sidecar path because closing another descriptor to it would drop this process's record locks.
pub(in crate::row_locks) struct FileLockCoordinator {
    file: std::fs::File,
    change_file: std::fs::File,
    claim_file: std::fs::File,
    sequence_file: std::fs::File,
    change_journal: Mutex<()>,
    transaction_xids: Mutex<xids::TransactionXids>,
    temporary_role_slots: Mutex<temporary_roles::Slots>,
    state: Mutex<CoordinatorState>,
}

mod claims;
mod journal;
mod platform;
mod relations;
mod row_claims;
mod sequence_positions;
mod temporary_roles;
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
        let mut claim_sidecar = database_path.as_os_str().to_owned();
        claim_sidecar.push(".uqa-row-claims");
        let claim_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&claim_sidecar)
            .map_err(|error| {
                format!(
                    "open cross-process row claim table `{}`: {error}",
                    Path::new(&claim_sidecar).display()
                )
            })?;
        let mut sequence_sidecar = database_path.as_os_str().to_owned();
        sequence_sidecar.push(".uqa-sequences");
        let sequence_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&sequence_sidecar)
            .map_err(|error| {
                format!(
                    "open cross-process sequence positions `{}`: {error}",
                    Path::new(&sequence_sidecar).display()
                )
            })?;
        let pid = std::process::id();
        let coordinator = Self {
            file,
            change_file,
            claim_file,
            sequence_file,
            change_journal: Mutex::new(()),
            transaction_xids: Mutex::new(xids::TransactionXids::new()),
            temporary_role_slots: Mutex::new(temporary_roles::Slots::default()),
            state: Mutex::new(CoordinatorState {
                claims: HashMap::new(),
                holders: HashMap::new(),
                wait_slots: HashMap::new(),
                holder_slots: HashMap::new(),
                pending_holders: HashMap::new(),
                pending_order: std::collections::BTreeMap::new(),
                next_pending: 0,
                occupied_holder_slots: vec![false; HOLDER_SLOT_COUNT as usize],
                released_holder_slots: Vec::new(),
                next_holder_slot: u64::from(pid).wrapping_mul(31) % HOLDER_SLOT_COUNT,
                rows: row_claims::RowClaims::default(),
            }),
        };
        Ok(coordinator)
    }
}

impl FileLockCoordinator {
    /// Whether sequence positions are kept in a sidecar every attached process reads.
    #[allow(clippy::unused_self)]
    pub(in crate::row_locks) const fn shares_sequence_positions(&self) -> bool {
        true
    }
}

impl Drop for FileLockCoordinator {
    fn drop(&mut self) {
        self.detach_transaction_xids();
        self.detach_row_claims_process();
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
            coordinator.clear_holder_slots(&mut state, session, std::slice::from_ref(claim));
        }
        assert!(state.holder_slots.is_empty());
        assert!(state.occupied_holder_slots.iter().all(|occupied| !occupied));
    }

    #[test]
    fn released_holder_slots_bound_file_growth_across_transactions() {
        let directory = tempfile::tempdir().unwrap();
        let coordinator = FileLockCoordinator::open(&directory.path().join("reuse.db")).unwrap();
        coordinator.state.lock().next_holder_slot = 0;
        for transaction in 0..4 {
            let claims = (0..128)
                .map(|ordinal| ByteClaim {
                    offset: 10_000 + transaction * 128 + ordinal,
                    write: true,
                })
                .collect::<Vec<_>>();
            assert!(matches!(coordinator.try_claim(17, &claims), Ok(Ok(()))));
            coordinator.publish_holders();
            assert_eq!(
                coordinator.file.metadata().unwrap().len(),
                HOLDER_SLOT_BASE + HOLDER_SLOT_SIZE * 128
            );
            coordinator.release(17, &claims);
            let state = coordinator.state.lock();
            assert!(state.holder_slots.is_empty());
            assert!(state.claims.is_empty());
            assert_eq!(state.next_holder_slot, 128);
            assert_eq!(state.released_holder_slots.len(), 128);
        }
    }

    #[test]
    fn holders_are_published_before_any_local_wait_is_advertised() {
        let directory = tempfile::tempdir().unwrap();
        let coordinator = FileLockCoordinator::open(&directory.path().join("pending.db")).unwrap();
        let held = [10_000, 10_001].map(|offset| ByteClaim {
            offset,
            write: true,
        });
        let released = ByteClaim {
            offset: 10_002,
            write: false,
        };
        assert!(matches!(coordinator.try_claim(17, &held), Ok(Ok(()))));
        assert!(matches!(coordinator.try_claim(17, &[released]), Ok(Ok(()))));
        coordinator.release(17, &[released]);
        {
            let state = coordinator.state.lock();
            assert!(state.holder_slots.is_empty(), "claims write no slot");
            assert!(
                state.released_holder_slots.is_empty(),
                "an unpublished release clears no slot"
            );
        }
        coordinator.register_wait(
            23,
            ByteClaim {
                offset: 20_000,
                write: true,
            },
        );
        let state = coordinator.state.lock();
        assert!(state.pending_holders.is_empty() && state.pending_order.is_empty());
        for claim in held {
            let index = state.holder_slots[&(17, claim.offset, claim.write)][0];
            let slot = coordinator.read_holder_slot(index).unwrap();
            assert_eq!((slot.session, slot.offset), (17, claim.offset));
        }
        assert!(!state
            .holder_slots
            .contains_key(&(17, released.offset, released.write)));
    }

    #[test]
    fn released_holes_are_reused_without_overwriting_live_holders() {
        let directory = tempfile::tempdir().unwrap();
        let coordinator = FileLockCoordinator::open(&directory.path().join("holes.db")).unwrap();
        coordinator.state.lock().next_holder_slot = 0;
        let first = (0..32)
            .map(|ordinal| ByteClaim {
                offset: 10_000 + ordinal,
                write: true,
            })
            .collect::<Vec<_>>();
        assert!(matches!(coordinator.try_claim(17, &first), Ok(Ok(()))));
        coordinator.publish_holders();
        for claim in first.iter().step_by(2) {
            coordinator.release(17, &[*claim]);
        }
        let second = (0..16)
            .map(|ordinal| ByteClaim {
                offset: 20_000 + ordinal,
                write: true,
            })
            .collect::<Vec<_>>();
        assert!(matches!(coordinator.try_claim(23, &second), Ok(Ok(()))));
        coordinator.publish_holders();
        {
            let state = coordinator.state.lock();
            for (ordinal, claim) in first.iter().enumerate().skip(1).step_by(2) {
                assert_eq!(
                    state.holder_slots[&(17, claim.offset, claim.write)],
                    [ordinal as u64]
                );
            }
            let reused = second
                .iter()
                .map(|claim| state.holder_slots[&(23, claim.offset, claim.write)][0])
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(reused, (0..32).step_by(2).collect());
            assert_eq!(state.next_holder_slot, 32);
        }
        assert_eq!(
            coordinator.file.metadata().unwrap().len(),
            HOLDER_SLOT_BASE + HOLDER_SLOT_SIZE * 32
        );
        for claim in first.iter().skip(1).step_by(2) {
            coordinator.release(17, &[*claim]);
        }
        coordinator.release(23, &second);
        assert!(coordinator.state.lock().holder_slots.is_empty());
    }
}
