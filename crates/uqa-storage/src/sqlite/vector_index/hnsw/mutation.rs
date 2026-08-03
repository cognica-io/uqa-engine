//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic vector and HNSW graph mutations.

use std::collections::BTreeMap;

use rusqlite::params;
use uqa_core::DocId;

use super::SQLiteHNSWIndex;
use crate::hnsw_index::HNSWIndex;
use crate::vector_index::VectorIndex;
use crate::{StorageBackendError, StorageBackendResult};

impl SQLiteHNSWIndex {
    pub(super) fn initialize_graph(&self) -> StorageBackendResult<()> {
        let expected_revision = self.persisted_revision()?;
        if self.require_persisted_graph && expected_revision.is_none() {
            return Err(missing_metadata(self));
        }
        let entries = self.persistent.load_all_with_ordinals()?;
        let mut graph = HNSWIndex::with_params(self.persistent.dimensions, self.params)?;
        let mut by_doc = BTreeMap::<DocId, Vec<(u32, Vec<f32>)>>::new();
        for (doc_id, ordinal, vector) in entries {
            by_doc.entry(doc_id).or_default().push((ordinal, vector));
        }
        for (doc_id, mut vectors) in by_doc {
            vectors.sort_by_key(|(ordinal, _)| *ordinal);
            graph.add_many(
                doc_id,
                vectors.into_iter().map(|(_, vector)| vector).collect(),
            )?;
        }
        let delta = graph.take_persistence_delta();
        let revision = next_revision(expected_revision)?;
        self.persistent.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            self.persist_delta(&tx, &delta, expected_revision, revision)?;
            tx.commit()?;
            Ok(())
        })?;
        self.publish_graph(graph, revision);
        Ok(())
    }

    pub(super) fn replace_document(
        &self,
        doc_id: DocId,
        vectors: Vec<Vec<f32>>,
    ) -> StorageBackendResult<()> {
        if self.persisted_revision()?.is_none() && !self.require_persisted_graph {
            let mut persistent = self.persistent.clone();
            return persistent.add_many(doc_id, vectors);
        }
        let (encoded_doc_id, encoded_vectors) =
            self.persistent.stage_doc_vectors(doc_id, &vectors)?;
        let cached = self.cached_graph_state()?;
        let mut graph = cached.graph.as_ref().clone();
        graph.add_many(doc_id, vectors)?;
        let delta = graph.take_persistence_delta();
        let revision = next_revision(Some(cached.revision))?;
        self.persistent.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(
                "DELETE FROM _vectors
                  WHERE table_name = ?1 AND field = ?2 AND doc_id = ?3",
                params![self.persistent.table, self.persistent.field, encoded_doc_id],
            )?;
            let mut statement = tx.prepare(
                "INSERT INTO _vectors
                    (table_name, field, doc_id, vector_ordinal, vector)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (ordinal, vector) in &encoded_vectors {
                statement.execute(params![
                    self.persistent.table,
                    self.persistent.field,
                    encoded_doc_id,
                    ordinal,
                    vector,
                ])?;
            }
            drop(statement);
            self.persist_delta(&tx, &delta, Some(cached.revision), revision)?;
            tx.commit()?;
            Ok(())
        })?;
        self.publish_graph(graph, revision);
        Ok(())
    }

    pub(super) fn delete_document(&self, doc_id: DocId) -> StorageBackendResult<()> {
        if self.persisted_revision()?.is_none() && !self.require_persisted_graph {
            let mut persistent = self.persistent.clone();
            return persistent.delete(doc_id);
        }
        let encoded_doc_id = i64::try_from(doc_id).map_err(|_| {
            StorageBackendError::Other(format!(
                "document id {doc_id} does not fit in SQLite INTEGER"
            ))
        })?;
        let cached = self.cached_graph_state()?;
        let mut graph = cached.graph.as_ref().clone();
        graph.delete(doc_id)?;
        let delta = graph.take_persistence_delta();
        let revision = next_revision(Some(cached.revision))?;
        self.persistent.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(
                "DELETE FROM _vectors
                  WHERE table_name = ?1 AND field = ?2 AND doc_id = ?3",
                params![self.persistent.table, self.persistent.field, encoded_doc_id],
            )?;
            self.persist_delta(&tx, &delta, Some(cached.revision), revision)?;
            tx.commit()?;
            Ok(())
        })?;
        self.publish_graph(graph, revision);
        Ok(())
    }

    pub(super) fn clear_graph(&self) -> StorageBackendResult<()> {
        if self.persisted_revision()?.is_none() && !self.require_persisted_graph {
            let mut persistent = self.persistent.clone();
            return persistent.clear();
        }
        let cached = self.cached_graph_state()?;
        let mut graph = cached.graph.as_ref().clone();
        graph.clear()?;
        let delta = graph.take_persistence_delta();
        let revision = next_revision(Some(cached.revision))?;
        self.persistent.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(
                "DELETE FROM _vectors WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            self.persist_delta(&tx, &delta, Some(cached.revision), revision)?;
            tx.commit()?;
            Ok(())
        })?;
        self.publish_graph(graph, revision);
        Ok(())
    }
}

pub(super) fn missing_metadata(index: &SQLiteHNSWIndex) -> StorageBackendError {
    StorageBackendError::Other(format!(
        "missing persisted HNSW metadata for {}.{}",
        index.persistent.table, index.persistent.field
    ))
}

fn next_revision(current: Option<u64>) -> StorageBackendResult<u64> {
    current
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| StorageBackendError::Other("HNSW metadata revision space exhausted".into()))
}
