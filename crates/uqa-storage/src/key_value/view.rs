//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compound readers and evaluated mutations share one provider-owned visibility boundary.

use std::sync::Arc;

use uqa_core::memory::BudgetedVec;

use crate::mvcc::{CommitSequence, DatabaseId, PrivateRecordRevision};
use crate::read_control::{
    KeyReadVisitor, KeyValueReadVisitor, StorageReadControl, ValueReadVisitor,
};
use crate::{KeyValueBatch, StorageBackendResult};

mod snapshot;

/// Copy a bounded key page before calling code that may read this same boundary again. Provider visitors can hold physical locks while lending their key bytes.
pub(crate) fn for_each_key(
    read: &dyn KeyValueRead,
    prefix: &[u8],
    visit: &mut dyn FnMut(&[u8]) -> StorageBackendResult<bool>,
) -> StorageBackendResult<()> {
    let mut after = None::<BudgetedVec<u8>>;
    loop {
        let mut keys = BudgetedVec::new(read.control().memory());
        read.visit_keys_after(prefix, after.as_deref(), 128, read.control(), &mut |key| {
            let mut owned = BudgetedVec::new(read.control().memory());
            owned.extend_from_slice(key)?;
            keys.push(owned)?;
            Ok(())
        })?;
        let Some(last) = keys.last() else {
            return Ok(());
        };
        let mut next = BudgetedVec::new(read.control().memory());
        next.extend_from_slice(last)?;
        after = Some(next);
        for key in keys.iter() {
            read.control().check()?;
            if !visit(key)? {
                return Ok(());
            }
        }
    }
}

pub type KeyValueReadScope<'a> = dyn FnMut(&dyn KeyValueRead) -> StorageBackendResult<()> + 'a;
pub type KeyValueMutation<'a> =
    dyn FnMut(&dyn KeyValueRead, &mut dyn KeyValueBatch) -> StorageBackendResult<()> + 'a;
pub type KeyValueVersionedMutation<'a> = dyn FnMut(
        crate::mvcc::StorageMutationOrigin,
        &dyn KeyValueRead,
        &mut dyn KeyValueBatch,
    ) -> StorageBackendResult<()>
    + 'a;

/// A fixed read boundary supplied by the store. Borrowed visitors must not reenter persistence; compound reads make successive calls through this same reader.
pub trait KeyValueRead {
    fn control(&self) -> &StorageReadControl;
    fn revision(&self, prefixes: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision>;
    /// Identity of one live record on this fixed view. Unlike a view revision, unrelated commits do not change it. Compare identities only for the same logical key; missing records and tombstones return `None`. Providers without exact committed/private record provenance must reject this capability.
    fn record_revision(&self, _key: &[u8]) -> StorageBackendResult<Option<KeyValueReadRevision>> {
        self.control().check()?;
        Err(super::codec::other_error(
            "individual record revisions are not supported",
        ))
    }
    /// Read source attached to this exact private metadata replacement. Absence never authorizes opening a newer snapshot. Committed values have no attachment; provider wrappers translate the metadata key but preserve the source's own key space and controls.
    fn retained_source(
        &self,
        _key: &[u8],
    ) -> StorageBackendResult<Option<Arc<dyn KeyValueRead + Send + Sync>>> {
        self.control().check()?;
        Ok(None)
    }
    /// Retain this committed/private boundary after the callback returns. Callers may only read the selected prefixes. The default copies selected bytes under this reader's allowance; versioned providers retain their existing visibility owners without loading values.
    fn retain(
        &self,
        prefixes: &[&[u8]],
    ) -> StorageBackendResult<Arc<dyn KeyValueRead + Send + Sync>> {
        snapshot::capture(self, prefixes)
    }
    fn visit_value(&self, key: &[u8], visit: &mut ValueReadVisitor<'_>)
        -> StorageBackendResult<()>;
    fn visit_prefix(
        &self,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()>;

    /// Visit one value on this same boundary, charging temporary provider buffers to the supplied query allowance.
    fn visit_value_budgeted(
        &self,
        _key: &[u8],
        control: &StorageReadControl,
        _visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        Err(super::codec::other_error(
            "controlled compound value reads are not supported",
        ))
    }

    /// Check a value's encoded size before provider materialization on this same committed/private view.
    fn visit_value_bounded(
        &self,
        _key: &[u8],
        _max_bytes: usize,
        control: &StorageReadControl,
        _visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        Err(super::codec::other_error(
            "size-bounded compound value reads are not supported",
        ))
    }

    /// Visit at most `limit` values in key order, strictly after `after`, without advancing this read boundary.
    fn visit_prefix_after(
        &self,
        _prefix: &[u8],
        _after: Option<&[u8]>,
        _limit: usize,
        control: &StorageReadControl,
        _visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        Err(super::codec::other_error(
            "controlled compound prefix reads are not supported",
        ))
    }

    /// Visit at most `limit` live keys in ascending order, strictly after `after`, without reading their values or advancing this boundary. Capable wrappers must preserve this key-only contract.
    fn visit_keys_after(
        &self,
        _prefix: &[u8],
        _after: Option<&[u8]>,
        _limit: usize,
        control: &StorageReadControl,
        _visit: &mut KeyReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        Err(super::codec::other_error(
            "compound key-only scans are not supported",
        ))
    }

    /// Probe live keys on this boundary without materializing their values.
    fn contains_prefix_budgeted(
        &self,
        _prefix: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        control.check()?;
        Err(super::codec::other_error(
            "compound key-only probes are not supported",
        ))
    }

    fn get(&self, key: &[u8]) -> StorageBackendResult<Option<BudgetedVec<u8>>> {
        let mut result = None;
        self.visit_value(key, &mut |value| {
            if let Some(value) = value {
                let mut bytes = BudgetedVec::new(self.control().memory());
                bytes.extend_from_slice(value)?;
                result = Some(bytes);
            }
            Ok(())
        })?;
        Ok(result)
    }
}

/// Opaque cache identity. Numeric index revisions alone do not distinguish undo branches or independent private transactions.
#[derive(Clone)]
pub struct KeyValueReadRevision(Revision);

#[derive(Clone)]
enum Revision {
    Memory(Arc<()>),
    Record(crate::mvcc::VisibleRecordRevision),
    Records {
        database: DatabaseId,
        committed: CommitSequence,
        private: Option<PrivateRecordRevision>,
    },
}

impl KeyValueReadRevision {
    pub(crate) fn observed_commit(&self, database: DatabaseId) -> Option<CommitSequence> {
        match &self.0 {
            Revision::Record(revision) => revision.committed(database),
            _ => None,
        }
    }

    /// Compare private changes for the same selected prefixes, independently of intervening committed writes. Unversioned or individual-record identities cannot establish this property.
    pub fn same_private_changes(&self, other: &Self) -> bool {
        matches!((&self.0, &other.0), (
            Revision::Records { database: a, private: ap, .. },
            Revision::Records { database: b, private: bp, .. }
        ) if a == b && ap == bp)
    }

    pub(crate) fn record_database(&self) -> Option<DatabaseId> {
        match &self.0 {
            Revision::Record(revision) => Some(revision.database()),
            Revision::Memory(_) | Revision::Records { .. } => None,
        }
    }

    /// Whether the selected record prefixes include changes private to this transaction. Unversioned view identities conservatively report private state because they cannot prove committed provenance.
    pub fn has_private_changes(&self) -> bool {
        match &self.0 {
            Revision::Memory(_) => true,
            Revision::Record(revision) => revision.is_private(),
            Revision::Records { private, .. } => private.is_some(),
        }
    }

    /// Allocate a distinct identity for a provider-owned view. Retain and clone it while that view is unchanged; allocate a fresh identity after writes or undo instead of reusing a numeric counter.
    pub fn fresh() -> Self {
        Self(Revision::Memory(Arc::new(())))
    }

    pub(crate) fn memory(identity: &Arc<()>) -> Self {
        Self(Revision::Memory(Arc::clone(identity)))
    }

    pub(crate) fn record(revision: crate::mvcc::VisibleRecordRevision) -> Self {
        Self(Revision::Record(revision))
    }

    pub(crate) fn records(
        database: DatabaseId,
        committed: CommitSequence,
        private: Option<PrivateRecordRevision>,
    ) -> Self {
        Self(Revision::Records {
            database,
            committed,
            private,
        })
    }
}

impl PartialEq for KeyValueReadRevision {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Revision::Memory(a), Revision::Memory(b)) => Arc::ptr_eq(a, b),
            (Revision::Record(a), Revision::Record(b)) => a == b,
            (
                Revision::Records {
                    database: a,
                    committed: ac,
                    private: ap,
                },
                Revision::Records {
                    database: b,
                    committed: bc,
                    private: bp,
                },
            ) => a == b && ac == bc && ap == bp,
            _ => false,
        }
    }
}
impl Eq for KeyValueReadRevision {}
