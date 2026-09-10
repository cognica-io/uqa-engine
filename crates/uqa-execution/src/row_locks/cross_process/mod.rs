//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cross-process row and relation lock coordination.
//!
//! Independent OS processes opening the same durable database coordinate logical locks through native byte-range locks on a sidecar file next to the database. Byte offsets derive from stable hashes of the relation name and row identity; hash collisions only make coordination more conservative, never less. Record locks die with the owning process, so a crashed process can never leave a stale logical lock behind.
//!
//! Each row maps to a two-byte range whose first byte carries key-related claims and whose second byte carries row-update claims. Mapping the four `PostgreSQL` tuple-lock strengths onto shared and exclusive claims of those two bytes reproduces the exact `PostgreSQL` 18 tuple-lock conflict matrix across processes:
//!
//! - `FOR KEY SHARE`: shared claim of the key byte.
//! - `FOR SHARE`: shared claim of the row byte.
//! - `FOR NO KEY UPDATE`: exclusive claim of the row byte.
//! - `FOR UPDATE`: exclusive claims of both bytes.
//!
//! Fixed slot tables at the start of the sidecar record the exact session holding or waiting for each byte. A waiter can therefore walk the cross-process wait-for graph and report `40P01` only when it reaches its own `(pid, session)`, mirroring `PostgreSQL`'s deadlock detector.

use uqa_sql::ast::LockStrength;

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

/// Sidecar layout. Coordination bytes and wait/holder slots occupy the low addresses; record-lock byte ranges for relations and rows start above them so lock offsets never alias structured data offsets.
const RELATION_BASE: u64 = 1 << 20;
const RELATION_SPAN: u64 = 1 << 20;
const ROW_BASE: u64 = 1 << 21;
const CHANGE_GATE_BYTE: u64 = 9;
/// Row byte pairs occupy `[ROW_BASE, ROW_BASE + 2 * ROW_SPAN)`. Record-lock offsets travel through `off_t`, so the span is sized to the platform's `off_t` width: 2^40 rows on 64-bit `off_t`, and the largest power of two that keeps every offset below `i32::MAX` where `off_t` is 32 bits.
const ROW_SPAN: u64 = row_span_for_offset_width(std::mem::size_of::<OffsetWidth>());

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
}

pub(super) const fn change_gate_claim(write: bool) -> ByteClaim {
    ByteClaim {
        offset: CHANGE_GATE_BYTE,
        write,
    }
}

pub(super) fn row_byte_claims(
    relation: &[u8],
    doc_id: uqa_core::DocId,
    strength: LockStrength,
) -> Vec<ByteClaim> {
    let base = ROW_BASE + (stable_hash(&[relation, &doc_id.to_be_bytes()]) % ROW_SPAN) * 2;
    match strength {
        LockStrength::ForKeyShare => vec![ByteClaim {
            offset: base,
            write: false,
        }],
        LockStrength::ForShare => vec![ByteClaim {
            offset: base + 1,
            write: false,
        }],
        LockStrength::ForNoKeyUpdate => vec![ByteClaim {
            offset: base + 1,
            write: true,
        }],
        LockStrength::ForUpdate => vec![
            ByteClaim {
                offset: base,
                write: true,
            },
            ByteClaim {
                offset: base + 1,
                write: true,
            },
        ],
    }
}

pub(super) fn relation_byte_claims(relation: &[u8], mode: RelationLockMode) -> Vec<ByteClaim> {
    let offset = RELATION_BASE + stable_hash(&[relation]) % RELATION_SPAN;
    vec![ByteClaim {
        offset,
        write: matches!(mode, RelationLockMode::AccessExclusive),
    }]
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
pub(super) use file::FileLockCoordinator;

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
// Native record locks and process-liveness probes have no stable safe wrapper in std. The unsafe surface is confined to operating-system calls over file handles, process handles, and their plain C data structures.
#[allow(unsafe_code)]
mod file;

#[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
pub(super) use fallback::FileLockCoordinator;

#[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
mod fallback {
    use std::path::Path;

    use super::ByteClaim;

    /// Sandboxed targets without native processes retain process-local lock semantics instead of rejecting every persistent mutation.
    pub(in crate::row_locks) struct FileLockCoordinator {}

    impl FileLockCoordinator {
        pub(in crate::row_locks) fn open(_database_path: &Path) -> Result<Self, String> {
            Ok(Self {})
        }

        pub(in crate::row_locks) fn try_claim(
            &self,
            _session: u64,
            _claims: &[ByteClaim],
        ) -> Result<Result<(), ByteClaim>, String> {
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

        pub(in crate::row_locks) fn change_sequence(&self) -> Result<u64, String> {
            Ok(0)
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
