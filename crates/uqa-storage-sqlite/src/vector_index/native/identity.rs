//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained vector generations distinguish committed data and non-reusable private changes.

use super::NativeVectorRead;
use crate::mvcc::native::{NativeRecordFamily as Family, NativeRecordIdentity, NativeRecordOwner};
use crate::Result;
use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::mvcc::{CommitSequence, PrivateRecordRevision};

#[derive(Clone, PartialEq, Eq)]
pub(in crate::vector_index) struct VectorIdentity {
    owner: NativeRecordOwner,
    committed: Option<CommitSequence>,
    private: Option<PrivateRecordRevision>,
}

pub(in crate::vector_index) fn identity(
    read: &NativeVectorRead<'_>,
    families: &[Family],
) -> Result<Option<VectorIdentity>> {
    let Some(owner) = read.owner else {
        return Ok(None);
    };
    let snapshot = read.snapshot;
    let key = NativeRecordIdentity::new(
        Family::CacheRevisions,
        NativeRecordOwner::Database(snapshot.database),
    )?
    .encode_key(
        &[
            ValueRef::Text(b"data"),
            ValueRef::Text(read.index.table.as_bytes()),
        ],
        &snapshot.control,
    )?;
    let committed = snapshot
        .view
        .committed()
        .metadata(&key, &snapshot.control)?
        .and_then(|meta| meta.revision);
    let mut private = None;
    for &family in families {
        let prefix = NativeRecordIdentity::new(family, owner)?
            .encode_prefix(&[read.field()], &snapshot.control)?;
        let mut after = BudgetedVec::new(snapshot.control.memory());
        loop {
            let page = snapshot.view.private_keys(
                &prefix,
                (!after.is_empty()).then_some(&*after),
                64,
                &snapshot.control,
            )?;
            let Some(last) = page.last() else { break };
            after.clear();
            after.extend_from_slice(last.key())?;
            for key in page.iter() {
                private = private.max(Some(key.revision()));
            }
        }
    }
    Ok(Some(VectorIdentity {
        owner,
        committed,
        private,
    }))
}
