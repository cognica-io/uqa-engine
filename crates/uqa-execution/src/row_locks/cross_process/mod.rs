//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cross-process row and relation lock coordination.
//!
//! Independent OS processes opening the same durable database coordinate logical locks through sidecar files next to the database. Relation identities are compared in full and assigned leased native lock slots. Holders and waiters pin their selected slot against reuse. Record locks die with the owning process, so a crashed process can never leave a stale logical lock behind.
//!
//! Row claims are entries of a shared claim table instead of record locks, because a statement may claim any number of rows and each record lock call walks every record lock of its file. An entry names its owning process and session, and the owning process holds one record lock, its liveness byte, for as long as it is attached, so an entry whose owner died is recognized and discarded.
//!
//! Each row has a key byte carrying key-related claims and a row byte carrying row-update claims. Mapping the four `PostgreSQL` tuple-lock strengths onto shared and exclusive claims of those two bytes reproduces the exact `PostgreSQL` 18 tuple-lock conflict matrix across processes:
//!
//! - `FOR KEY SHARE`: shared claim of the key byte.
//! - `FOR SHARE`: shared claim of the row byte.
//! - `FOR NO KEY UPDATE`: exclusive claim of the row byte.
//! - `FOR UPDATE`: exclusive claims of both bytes.
//!
//! The claim table names the exact session holding each row byte, and fixed slot tables at the start of the lock sidecar record the session holding each relation byte and the byte each session waits for. A waiter can therefore walk the cross-process wait-for graph and report `40P01` only when it reaches its own `(pid, session)`, mirroring `PostgreSQL`'s deadlock detector.

use uqa_sql::ast::LockStrength;

#[cfg_attr(
    not(any(windows, all(unix, not(target_os = "emscripten")))),
    allow(dead_code)
)]
mod row_identity;
pub(super) use row_identity::RowIdentity;

use super::{PhysicalRowChangeTarget, RelationLockMode, RowChangeTarget};

#[derive(Clone, Copy, Debug)]
#[cfg_attr(
    not(any(windows, all(unix, not(target_os = "emscripten")))),
    allow(dead_code)
)]
pub(super) struct PublishedRowChange {
    pub table_hash: u64,
    pub doc_id: u64,
    pub kind: PublishedRowChangeKind,
    pub strength: LockStrength,
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(
    not(any(windows, all(unix, not(target_os = "emscripten")))),
    allow(dead_code)
)]
pub(super) struct PublishedRowIdentity {
    pub table_hash: u64,
    pub doc_id: u64,
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(
    not(any(windows, all(unix, not(target_os = "emscripten")))),
    allow(dead_code)
)]
pub(super) enum PublishedRowChangeKind {
    Update,
    Delete,
    Rewrite(PublishedRowIdentity),
}

/// Sidecar layout. Coordination bytes and wait/holder slots occupy the low addresses; record-lock byte ranges start above them so lock offsets never alias structured data offsets.
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
const RELATION_BASE: u64 = 1 << 20;
const RELATION_SPAN: u64 = 1 << 20;
const ROW_BASE: u64 = 1 << 21;
const CHANGE_GATE_BYTE: u64 = 9;
/// `[ROW_BASE, ROW_BASE + 2 * ROW_SPAN)` carried one record lock per row byte before row claims moved to the shared claim table. The range stays reserved so the relation mode bytes above it keep their offsets. Record-lock offsets travel through `off_t`, so the span is sized to the platform's `off_t` width: 2^40 rows on 64-bit `off_t`, and the largest power of two that keeps every offset below `i32::MAX` where `off_t` is 32 bits.
const ROW_SPAN: u64 = row_span_for_offset_width(std::mem::size_of::<OffsetWidth>());
const RELATION_MODE_BASE: u64 = ROW_BASE + 2 * ROW_SPAN;
const RELATION_WAIT_BASE: u64 = RELATION_MODE_BASE + 8 * RELATION_SPAN;
/// One liveness byte for each process attached to the row claim table, which only the file coordinator keeps.
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
const PROCESS_LIVENESS_BASE: u64 = RELATION_WAIT_BASE + 8 * RELATION_SPAN;
/// Row descriptors set the top bit, which no record-lock offset uses. The low bit selects the key/row mode; complete identity travels separately and never depends on this descriptor.
const ROW_CLAIM: u64 = 1 << 63;

#[cfg(all(unix, not(target_os = "emscripten")))]
type OffsetWidth = libc::off_t;
#[cfg(not(all(unix, not(target_os = "emscripten"))))]
type OffsetWidth = i64;

const fn row_span_for_offset_width(bytes: usize) -> u64 {
    if bytes >= 8 {
        1 << 40
    } else {
        // (i32::MAX - ROW_BASE) / 2 rounded down to a power of two.
        1 << 29
    }
}

/// One advisory byte claim: `write` claims the byte exclusively.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ByteClaim {
    pub offset: u64,
    pub write: bool,
    pub row: Option<RowIdentity>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(any(windows, all(unix, not(target_os = "emscripten")))),
    allow(dead_code)
)]
pub(super) enum RelationClaimWait {
    AdmissionBusy,
    Conflict(ByteClaim),
}

pub(super) const fn change_gate_claim(write: bool) -> ByteClaim {
    ByteClaim {
        offset: CHANGE_GATE_BYTE,
        write,
        row: None,
    }
}

/// One of the two claimable bytes of a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(any(windows, all(unix, not(target_os = "emscripten")))),
    allow(dead_code)
)]
pub(super) enum RowByte {
    Key,
    Row,
}

/// The row identity and byte a claim addresses, or `None` for a record-lock claim.
#[cfg_attr(
    not(any(windows, all(unix, not(target_os = "emscripten")))),
    allow(dead_code)
)]
pub(super) fn row_claim_address(claim: ByteClaim) -> Option<(RowIdentity, RowByte)> {
    claim.row.map(|identity| {
        (
            identity,
            if claim.offset & 1 == 0 {
                RowByte::Key
            } else {
                RowByte::Row
            },
        )
    })
}

pub(super) fn row_claim(identity: RowIdentity, byte: RowByte, write: bool) -> ByteClaim {
    ByteClaim {
        offset: ROW_CLAIM | u64::from(byte == RowByte::Row),
        write,
        row: Some(identity),
    }
}

pub(super) fn row_byte_claims(identity: RowIdentity, strength: LockStrength) -> Vec<ByteClaim> {
    match strength {
        LockStrength::ForKeyShare => vec![row_claim(identity, RowByte::Key, false)],
        LockStrength::ForShare => vec![row_claim(identity, RowByte::Row, false)],
        LockStrength::ForNoKeyUpdate => vec![row_claim(identity, RowByte::Row, true)],
        LockStrength::ForUpdate => vec![
            row_claim(identity, RowByte::Key, true),
            row_claim(identity, RowByte::Row, true),
        ],
    }
}

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub(super) fn relation_byte_claims(relation: u64, mode: RelationLockMode) -> [ByteClaim; 1] {
    [relation_mode_claim(relation, mode, false)]
}

/// Each held mode occupies its own shared byte. Admission checks incompatible bytes exclusively before publishing a new holder; this also represents mutually conflicting modes that are individually self-compatible.
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub(super) fn relation_mode_claim(relation: u64, mode: RelationLockMode, write: bool) -> ByteClaim {
    ByteClaim {
        offset: RELATION_MODE_BASE + relation * 8 + mode as u64,
        write,
        row: None,
    }
}

/// A wait descriptor names the complete requested mode, including every conflicting holder. This address is metadata only and is never claimed as a native lock byte.
pub(super) fn relation_wait_claim(relation: u64, mode: RelationLockMode) -> ByteClaim {
    ByteClaim {
        offset: RELATION_WAIT_BASE + relation * 8 + mode as u64,
        write: true,
        row: None,
    }
}

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
fn relation_slot_of_claim(offset: u64) -> Option<u64> {
    [RELATION_MODE_BASE, RELATION_WAIT_BASE]
        .into_iter()
        .find_map(|base| {
            (base..base + 8 * RELATION_SPAN)
                .contains(&offset)
                .then(|| (offset - base) / 8)
        })
}

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub(super) fn wait_blocking_claims(wanted: ByteClaim) -> impl Iterator<Item = ByteClaim> + Clone {
    let mut claims = [None; 8];
    if (RELATION_WAIT_BASE..RELATION_WAIT_BASE + 8 * RELATION_SPAN).contains(&wanted.offset) {
        let position = wanted.offset - RELATION_WAIT_BASE;
        let mode = RelationLockMode::ALL[(position % 8) as usize];
        let relation = position / 8;
        for (index, held) in RelationLockMode::ALL.into_iter().enumerate() {
            if mode.conflicts_with(held) {
                claims[index] = Some(ByteClaim {
                    offset: RELATION_MODE_BASE + relation * 8 + index as u64,
                    write: true,
                    row: None,
                });
            }
        }
    } else {
        claims[0] = Some(wanted);
    }
    claims.into_iter().flatten()
}

/// Stable identity of a structural relation lock target shared by every process.
pub(super) fn table_hash(relation: &[u8]) -> u64 {
    stable_hash(&[relation])
}

/// FNV-1a: the offsets must be identical in every process, so the hash key cannot be process-random.
fn stable_hash(parts: &[&[u8]]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for part in parts {
        for byte in *part {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub(super) use file::{FileLockCoordinator, RelationIdentityLease};

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub(super) use file::journal::JournalReadLease;

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
// Native record locks and process-liveness probes have no stable safe wrapper in std. The unsafe surface is confined to operating-system calls over file handles, process handles, and their plain C data structures.
#[allow(unsafe_code)]
mod file;

#[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
pub(super) use fallback::{FileLockCoordinator, RelationIdentityLease};

#[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
pub(super) use fallback::JournalReadLease;

#[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
mod fallback {
    use std::path::Path;

    #[derive(Debug)]
    pub(in crate::row_locks) struct JournalReadLease;

    use super::super::sequence_positions::{
        RecordedSequencePosition, SequencePosition, SequencePositionKey, SequenceSlot,
    };
    use super::{ByteClaim, RelationClaimWait, RelationLockMode, RowIdentity};

    /// Sandboxed targets without native processes retain process-local lock semantics instead of rejecting every persistent mutation.
    pub(in crate::row_locks) struct FileLockCoordinator {}

    pub(in crate::row_locks) struct RelationIdentityLease<'a>(std::marker::PhantomData<&'a ()>);
    impl RelationIdentityLease<'_> {
        pub(in crate::row_locks) fn slot(&self) -> u64 {
            0
        }
        pub(in crate::row_locks) fn retain(&mut self) {}
    }

    pub(in crate::row_locks) struct RowIdentityLease;
    impl RowIdentityLease {
        pub(in crate::row_locks) fn identity(&self) -> RowIdentity {
            RowIdentity::Relation {
                generation: 1,
                doc_id: 0,
            }
        }
        pub(in crate::row_locks) fn retain(&mut self) {}
    }

    impl FileLockCoordinator {
        pub(in crate::row_locks) fn open_with_key(
            _database_path: &Path,
            _key: Option<uqa_storage::StorageEncryptionKey>,
        ) -> Result<Self, String> {
            Ok(Self {})
        }

        pub(in crate::row_locks) fn pin_row<'a>(
            &'a self,
            _relation: &'a [u8],
            _doc_id: u64,
            cancel: &uqa_core::CancellationToken,
        ) -> Result<RowIdentityLease, uqa_sql::SQLError> {
            cancel.check()?;
            Ok(RowIdentityLease)
        }
        pub(in crate::row_locks) fn retained_row_identity(
            &self,
            _relation: &[u8],
            _doc_id: u64,
        ) -> Option<RowIdentity> {
            None
        }
        pub(in crate::row_locks) fn release_row_identity(&self, _relation: &[u8]) {}

        pub(in crate::row_locks) fn pin_relation<'a>(
            &'a self,
            _relation: &'a [u8],
            cancel: &uqa_core::CancellationToken,
        ) -> Result<RelationIdentityLease<'a>, uqa_sql::SQLError> {
            cancel.check()?;
            Ok(RelationIdentityLease(std::marker::PhantomData))
        }

        pub(in crate::row_locks) fn release_relation(
            &self,
            _session: u64,
            _relation: &[u8],
            _mode: RelationLockMode,
        ) {
        }

        pub(in crate::row_locks) fn retain_temporary_role(
            &self,
            _session: u64,
            _role: u32,
            cancel: &uqa_core::CancellationToken,
        ) -> Result<(), uqa_sql::SQLError> {
            cancel.check()?;
            Ok(())
        }

        pub(in crate::row_locks) fn release_temporary_role(&self, _session: u64, _role: u32) {}

        /// Without a sidecar the positions stay in the lock manager of this process.
        #[allow(clippy::unused_self)]
        pub(in crate::row_locks) const fn shares_sequence_positions(&self) -> bool {
            false
        }

        pub(in crate::row_locks) fn lock_sequence_positions(&self) -> Result<(), String> {
            Ok(())
        }

        pub(in crate::row_locks) fn unlock_sequence_positions(&self) {}

        pub(in crate::row_locks) fn read_sequence_slot(
            &self,
            _key: &SequencePositionKey,
        ) -> Result<SequenceSlot, String> {
            Err("sequence positions are not shared on this target".into())
        }

        pub(in crate::row_locks) fn record_sequence_position(
            &self,
            _key: &SequencePositionKey,
            _position: &SequencePosition,
        ) -> Result<bool, String> {
            Err("sequence positions are not shared on this target".into())
        }

        pub(in crate::row_locks) fn remove_sequence_position(
            &self,
            _key: &SequencePositionKey,
        ) -> Result<(), String> {
            Err("sequence positions are not shared on this target".into())
        }

        pub(in crate::row_locks) fn read_sequence_positions(
            &self,
        ) -> Result<Vec<(SequencePositionKey, RecordedSequencePosition)>, String> {
            Err("sequence positions are not shared on this target".into())
        }

        pub(in crate::row_locks) fn retain_sequence_positions(
            &self,
            _keep: &dyn Fn(&SequencePositionKey) -> bool,
        ) -> Result<(), String> {
            Err("sequence positions are not shared on this target".into())
        }

        pub(in crate::row_locks) fn foreign_temporary_role_reference(
            &self,
            _role: u32,
            cancel: &uqa_core::CancellationToken,
        ) -> Result<bool, uqa_sql::SQLError> {
            cancel.check()?;
            Ok(false)
        }

        pub(in crate::row_locks) fn try_claim(
            &self,
            _session: u64,
            _claims: &[ByteClaim],
        ) -> Result<Result<(), ByteClaim>, String> {
            Ok(Ok(()))
        }

        pub(in crate::row_locks) fn try_slot_claim(
            &self,
            _session: u64,
            _relation: u64,
            _mode: RelationLockMode,
        ) -> Result<Result<(), RelationClaimWait>, String> {
            Ok(Ok(()))
        }

        pub(in crate::row_locks) fn release(&self, _session: u64, _claims: &[ByteClaim]) {}

        pub(in crate::row_locks) fn register_wait(&self, _session: u64, _claim: ByteClaim) {}

        pub(in crate::row_locks) fn clear_wait(&self, _session: u64) {}

        pub(in crate::row_locks) fn wait_cycle_reaches_session(
            &self,
            _session: u64,
            _wanted: ByteClaim,
            _local_wait: &dyn Fn(u64) -> Option<ByteClaim>,
        ) -> bool {
            false
        }

        pub(in crate::row_locks) fn publish_changes(
            &self,
            _changes: &[super::PublishedRowChange],
        ) -> Result<(), String> {
            Ok(())
        }

        pub(in crate::row_locks) fn pin_change_sequence(
            self: &std::sync::Arc<Self>,
        ) -> Result<(u64, std::sync::Arc<JournalReadLease>), String> {
            Ok((0, std::sync::Arc::new(JournalReadLease)))
        }

        pub(in crate::row_locks) fn allocate_transaction_xid(&self) -> Result<Option<u32>, String> {
            Ok(None)
        }

        pub(in crate::row_locks) fn change_target_after(
            &self,
            _table_hash: u64,
            _doc_id: u64,
            _baseline: u64,
            _wanted: uqa_sql::ast::LockStrength,
        ) -> Result<super::RowChangeTarget, String> {
            Ok(super::RowChangeTarget::Unchanged)
        }

        pub(in crate::row_locks) fn physical_change_target_after(
            &self,
            _table_hash: u64,
            _doc_id: u64,
            _baseline: u64,
            _wanted: uqa_sql::ast::LockStrength,
        ) -> Result<super::PhysicalRowChangeTarget, String> {
            Ok(super::PhysicalRowChangeTarget::Unchanged)
        }
    }
}
