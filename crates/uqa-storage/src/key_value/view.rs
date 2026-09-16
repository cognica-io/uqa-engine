//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compound readers and evaluated mutations share one provider-owned visibility boundary.

use std::sync::Arc;

use uqa_core::memory::BudgetedVec;

use crate::mvcc::{CommitSequence, DatabaseId, PrivateRecordRevision};
use crate::read_control::{KeyValueReadVisitor, StorageReadControl, ValueReadVisitor};
use crate::{KeyValueBatch, StorageBackendResult};

pub type KeyValueReadScope<'a> = dyn FnMut(&dyn KeyValueRead) -> StorageBackendResult<()> + 'a;
pub type KeyValueMutation<'a> =
    dyn FnMut(&dyn KeyValueRead, &mut dyn KeyValueBatch) -> StorageBackendResult<()> + 'a;

/// A fixed read boundary supplied by the store. Borrowed visitors must not reenter persistence; compound reads make successive calls through this same reader.
pub trait KeyValueRead {
    fn control(&self) -> &StorageReadControl;
    fn revision(&self, prefixes: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision>;
    fn visit_value(&self, key: &[u8], visit: &mut ValueReadVisitor<'_>)
        -> StorageBackendResult<()>;
    fn visit_prefix(
        &self,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()>;

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
    Records {
        database: DatabaseId,
        committed: CommitSequence,
        private: Option<PrivateRecordRevision>,
    },
}

impl KeyValueReadRevision {
    /// Allocate a distinct identity for a provider-owned view. Retain and clone it while that view is unchanged; allocate a fresh identity after writes or undo instead of reusing a numeric counter.
    pub fn fresh() -> Self {
        Self(Revision::Memory(Arc::new(())))
    }

    pub(crate) fn memory(identity: &Arc<()>) -> Self {
        Self(Revision::Memory(Arc::clone(identity)))
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
