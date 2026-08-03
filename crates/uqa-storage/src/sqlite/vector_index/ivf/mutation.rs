//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic vector and IVF-metadata mutations.

use rusqlite::params;
use uqa_core::DocId;

use super::metadata::{encode_metadata, EncodedIVFMetadata};
use super::writing::write_metadata;
use super::SQLiteIVFIndex;
use crate::sqlite::vector_index::codec::encode_doc_id;
use crate::sqlite::SQLiteError;
use crate::StorageBackendResult;

impl SQLiteIVFIndex {
    pub(super) fn replace_document(
        &self,
        doc_id: DocId,
        vectors: Vec<Vec<f32>>,
    ) -> StorageBackendResult<()> {
        let (encoded_doc_id, encoded_vectors) =
            self.persistent.stage_doc_vectors(doc_id, &vectors)?;
        let mut prospective = self.persistent.load_all_with_ordinals()?;
        prospective.retain(|(stored_doc_id, _, _)| *stored_doc_id != doc_id);
        for (ordinal, vector) in vectors.into_iter().enumerate() {
            prospective.push((
                doc_id,
                u32::try_from(ordinal).map_err(|_| {
                    SQLiteError::StorageBackend(
                        "vector ordinal exceeds the u32 index format".into(),
                    )
                })?,
                vector,
            ));
        }
        prospective.sort_by_key(|(doc_id, ordinal, _)| (*doc_id, *ordinal));
        let snapshot = self.metadata_for_entries(&prospective)?;
        let metadata = encode_metadata(self.params, &snapshot)?;
        self.apply_replacement(encoded_doc_id, &encoded_vectors, &metadata)
    }

    pub(super) fn delete_document(&self, doc_id: DocId) -> StorageBackendResult<()> {
        let encoded_doc_id = encode_doc_id(doc_id)?;
        let mut prospective = self.persistent.load_all_with_ordinals()?;
        let previous_len = prospective.len();
        prospective.retain(|(stored_doc_id, _, _)| *stored_doc_id != doc_id);
        if prospective.len() == previous_len {
            return Ok(());
        }
        let snapshot = self.metadata_for_entries(&prospective)?;
        let metadata = encode_metadata(self.params, &snapshot)?;
        self.apply_delete(encoded_doc_id, &metadata)
    }

    pub(super) fn clear_index(&self) -> StorageBackendResult<()> {
        self.persistent.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            for table in [
                "_vectors",
                "_ivf_assignments",
                "_ivf_centroids",
                "_ivf_indexes",
            ] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE table_name = ?1 AND field = ?2"),
                    params![self.persistent.table, self.persistent.field],
                )?;
            }
            tx.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    fn apply_replacement(
        &self,
        doc_id: i64,
        vectors: &[(i64, Vec<u8>)],
        metadata: &EncodedIVFMetadata,
    ) -> StorageBackendResult<()> {
        self.persistent.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(
                "DELETE FROM _vectors
                  WHERE table_name = ?1 AND field = ?2 AND doc_id = ?3",
                params![self.persistent.table, self.persistent.field, doc_id],
            )?;
            let mut insert = tx.prepare(
                "INSERT INTO _vectors
                    (table_name, field, doc_id, vector_ordinal, vector)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (ordinal, vector) in vectors {
                insert.execute(params![
                    self.persistent.table,
                    self.persistent.field,
                    doc_id,
                    ordinal,
                    vector
                ])?;
            }
            drop(insert);
            write_metadata(&tx, &self.persistent, metadata)?;
            tx.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    fn apply_delete(&self, doc_id: i64, metadata: &EncodedIVFMetadata) -> StorageBackendResult<()> {
        self.persistent.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(
                "DELETE FROM _vectors
                  WHERE table_name = ?1 AND field = ?2 AND doc_id = ?3",
                params![self.persistent.table, self.persistent.field, doc_id],
            )?;
            write_metadata(&tx, &self.persistent, metadata)?;
            tx.commit()?;
            Ok(())
        })?;
        Ok(())
    }
}
