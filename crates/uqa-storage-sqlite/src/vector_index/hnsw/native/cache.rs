//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cache identities combine durable data revisions with non-reusable private batch identities.

use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::{
    hnsw_index::HNSWIndex,
    mvcc::{CommitSequence, PrivateRecordRevision},
    ReadOnlySnapshot,
};

use super::super::{CachedGraph, GraphIdentity as CacheIdentity};
use super::{load_meta, loading, SQLiteHNSWIndex};
use crate::mvcc::native::{NativeRecordFamily as Family, NativeRecordIdentity, NativeRecordOwner};
use crate::vector_index::native::NativeVectorRead;
use crate::Result;

#[derive(Clone, PartialEq, Eq)]
pub(in crate::vector_index) struct GraphIdentity {
    owner: NativeRecordOwner,
    committed: Option<CommitSequence>,
    private: Option<PrivateRecordRevision>,
}

impl SQLiteHNSWIndex {
    pub(in crate::vector_index::hnsw) fn cached_native_graph(
        &self,
        read: &NativeVectorRead<'_>,
    ) -> Result<Option<ReadOnlySnapshot<HNSWIndex>>> {
        read.snapshot.control.check()?;
        let Some(meta) = load_meta(read)? else {
            return Ok(None);
        };
        self.validate_header(meta.0, meta.1)?;
        let Some(identity) = identity(read)? else {
            return Ok(None);
        };
        if let Some(cached) = self.graph.read().as_ref() {
            if matches!(&cached.identity, CacheIdentity::Native(view) if view == &identity) {
                return Ok(Some(cached.graph.clone()));
            }
        }
        let graph = ReadOnlySnapshot::from_budgeted(loading::load_graph(read, meta)?)?;
        *self.graph.write() = Some(CachedGraph {
            revision: meta.3,
            identity: CacheIdentity::Native(identity),
            graph: graph.clone(),
        });
        Ok(Some(graph))
    }
}

fn identity(read: &NativeVectorRead<'_>) -> Result<Option<GraphIdentity>> {
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
    for family in [
        Family::Vectors,
        Family::HNSWIndexes,
        Family::HNSWNodes,
        Family::HNSWEdges,
    ] {
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
    Ok(Some(GraphIdentity {
        owner,
        committed,
        private,
    }))
}
