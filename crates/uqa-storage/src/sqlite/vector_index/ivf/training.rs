//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! IVF training and metadata state transitions.

use std::collections::BTreeMap;

use rusqlite::params;
use uqa_core::DocId;

use super::metadata::{encode_metadata, SQLiteIVFMeta};
use super::writing::{write_metadata, write_untrained_meta};
use super::SQLiteIVFIndex;
use crate::ivf_index::{IVFIndex, IVFMetadataSnapshot, IVFState};
use crate::sqlite::vector_index::codec::validate_persisted_ordinal_sequence;
use crate::vector_index::VectorIndex;
use crate::StorageBackendResult;

impl SQLiteIVFIndex {
    pub(super) fn ready_meta(&self) -> StorageBackendResult<Option<SQLiteIVFMeta>> {
        let Some(mut meta) = self.load_meta()? else {
            return Ok(None);
        };
        if meta.state == IVFState::Trained
            && (meta.dimensions != self.persistent.dimensions
                || meta.params != self.params
                || meta.vector_count != self.persistent.count()?)
        {
            meta.state = IVFState::Stale;
        }
        Ok(Some(meta))
    }

    pub(super) fn initialize_metadata(&self) -> StorageBackendResult<()> {
        if self.persistent.count()? >= self.params.train_threshold {
            self.train_metadata()
        } else {
            self.save_untrained_metadata(self.persistent.count()?)
        }
    }

    fn train_metadata(&self) -> StorageBackendResult<()> {
        let entries = self.persistent.load_all_with_ordinals()?;
        let snapshot = self.metadata_for_entries(&entries)?;
        self.save_snapshot(&snapshot)
    }

    pub(super) fn metadata_for_entries(
        &self,
        entries: &[(DocId, u32, Vec<f32>)],
    ) -> StorageBackendResult<IVFMetadataSnapshot> {
        validate_persisted_ordinal_sequence(entries)?;
        if entries.len() < self.params.train_threshold {
            return Ok(IVFMetadataSnapshot {
                state: IVFState::Untrained,
                centroids: Vec::new(),
                assignments: Vec::new(),
                trained_size: 0,
                deletes_since_train: 0,
                vector_count: entries.len(),
            });
        }
        let mut index = IVFIndex::with_params(
            self.persistent.dimensions,
            self.params.nlist,
            self.params.nprobe,
            self.params.train_threshold,
        );
        let mut by_doc = BTreeMap::<DocId, Vec<(u32, Vec<f32>)>>::new();
        for (doc_id, ordinal, vector) in entries {
            by_doc
                .entry(*doc_id)
                .or_default()
                .push((*ordinal, vector.clone()));
        }
        for (doc_id, mut vectors) in by_doc {
            vectors.sort_by_key(|(ordinal, _)| *ordinal);
            index.add_many(
                doc_id,
                vectors.into_iter().map(|(_, vector)| vector).collect(),
            )?;
        }
        index.train()?;
        Ok(index.metadata_snapshot())
    }

    fn save_snapshot(&self, snapshot: &IVFMetadataSnapshot) -> StorageBackendResult<()> {
        if snapshot.state == IVFState::Untrained {
            return self.save_untrained_metadata(snapshot.vector_count);
        }
        let metadata = encode_metadata(self.params, snapshot)?;
        self.persistent.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            write_metadata(&tx, &self.persistent, &metadata)?;
            tx.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    fn save_untrained_metadata(&self, vector_count: usize) -> StorageBackendResult<()> {
        self.persistent.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(
                "DELETE FROM _ivf_centroids WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            tx.execute(
                "DELETE FROM _ivf_assignments WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            write_untrained_meta(&tx, &self.persistent, self.params, vector_count)?;
            tx.commit()?;
            Ok(())
        })?;
        Ok(())
    }
}
