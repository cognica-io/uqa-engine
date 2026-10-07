//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cache identities combine durable data revisions with non-reusable private batch identities.

use uqa_storage::{hnsw_index::HNSWIndex, ReadOnlySnapshot};

use super::super::{CachedGraph, GraphIdentity as CacheIdentity};
use super::{load_meta, loading, SQLiteHNSWIndex};
use crate::mvcc::native::NativeRecordFamily as Family;
use crate::vector_index::native::NativeVectorRead;
use crate::Result;

pub(in crate::vector_index) use crate::vector_index::native::identity::VectorIdentity as GraphIdentity;

impl SQLiteHNSWIndex {
    pub(super) fn retain_native_candidate(
        &self,
        candidate: Option<super::Candidate>,
        snapshot: Option<&crate::mvcc::native::NativeSnapshot>,
        committed: Option<uqa_storage::mvcc::CommitSequence>,
    ) {
        *self.graph.write() = None;
        let retained = (|| {
            let (candidate, revision) = candidate?;
            let read = NativeVectorRead::new(snapshot?, &self.persistent).ok()?;
            let mut identity = crate::vector_index::native::identity::identity(
                &read,
                &[
                    Family::Vectors,
                    Family::HNSWIndexes,
                    Family::HNSWNodes,
                    Family::HNSWEdges,
                ],
            )
            .ok()??;
            if let Some(committed) = committed {
                identity = identity.after_uncontended_commit(committed);
            }
            Some(CachedGraph {
                revision,
                identity: CacheIdentity::Native(identity),
                graph: ReadOnlySnapshot::from_budgeted(candidate).ok()?,
            })
        })();
        *self.graph.write() = retained;
    }

    pub(in crate::vector_index::hnsw) fn cached_native_graph(
        &self,
        read: &NativeVectorRead<'_>,
    ) -> Result<Option<ReadOnlySnapshot<HNSWIndex>>> {
        read.snapshot.control.check()?;
        let Some(meta) = load_meta(read)? else {
            return Ok(None);
        };
        self.validate_header(meta.0, meta.1)?;
        let Some(identity) = crate::vector_index::native::identity::identity(
            read,
            &[
                Family::Vectors,
                Family::HNSWIndexes,
                Family::HNSWNodes,
                Family::HNSWEdges,
            ],
        )?
        else {
            return Ok(None);
        };
        if let Some(cached) = self.graph.read().as_ref() {
            if matches!(&cached.identity, CacheIdentity::Native(view) if view == &identity) {
                return Ok(Some(
                    cached
                        .graph
                        .clone()
                        .with_vector_read_control(&read.snapshot.control)?,
                ));
            }
        }
        let graph = ReadOnlySnapshot::from_budgeted(loading::load_graph(read, meta)?)?;
        let reader = graph
            .clone()
            .with_vector_read_control(&read.snapshot.control)?;
        *self.graph.write() = Some(CachedGraph {
            revision: meta.3,
            identity: CacheIdentity::Native(identity),
            graph,
        });
        Ok(Some(reader))
    }
}
