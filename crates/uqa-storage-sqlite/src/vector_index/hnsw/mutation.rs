//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! HNSW candidates are read, evaluated and persisted inside the same physical view.

use std::collections::BTreeMap;

use rusqlite::{params, Connection};
use uqa_core::DocId;

use super::{loading::load_meta_from, SQLiteHNSWIndex};
use crate::vector_index::{encode_doc_id, EncodedVector};
use crate::Result;
use uqa_storage::hnsw_index::HNSWIndex;
use uqa_storage::vector_index::VectorIndex;
use uqa_storage::{StorageBackendError, StorageBackendResult};

impl SQLiteHNSWIndex {
    pub(super) fn initialize_graph(&self) -> StorageBackendResult<()> {
        if self
            .persistent
            .write_native(|read, batch| self.initialize_native(read, batch))?
            .is_some()
        {
            return Ok(());
        }
        let ((graph, revision), identity) =
            self.persistent.conn.with_snapshot(|connection, _| {
                let expected =
                    load_meta_from(connection, self)?.map(|(_, _, _, revision)| revision);
                if self.require_persisted_graph && expected.is_none() {
                    return Err(missing_metadata(self).into());
                }
                let entries = self.persistent.load_all_with_ordinals_from(connection)?;
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
                let revision = next_revision(expected)?;
                self.persist_delta(connection, &delta, expected, revision)?;
                Ok((graph, revision))
            })?;
        self.publish_graph(graph, revision, identity);
        Ok(())
    }

    pub(super) fn replace_document(
        &self,
        doc_id: DocId,
        vectors: Vec<Vec<f32>>,
    ) -> StorageBackendResult<()> {
        let (encoded_doc_id, encoded_vectors) =
            self.persistent.stage_doc_vectors(doc_id, &vectors)?;
        if self
            .persistent
            .write_native(|read, batch| {
                self.mutate_native(
                    read,
                    batch,
                    |graph| graph.add_many(doc_id, vectors.clone()),
                    |read, batch| read.replace(batch, encoded_doc_id, &encoded_vectors),
                )
            })?
            .is_some()
        {
            return Ok(());
        }
        self.mutate_graph(
            |graph| graph.add_many(doc_id, vectors),
            |connection| replace_canonical(connection, self, encoded_doc_id, &encoded_vectors),
        )
    }

    pub(super) fn delete_document(&self, doc_id: DocId) -> StorageBackendResult<()> {
        let encoded = encode_doc_id(doc_id)?;
        if self
            .persistent
            .write_native(|read, batch| {
                self.mutate_native(
                    read,
                    batch,
                    |graph| graph.delete(doc_id),
                    |read, batch| read.delete(batch, encoded),
                )
            })?
            .is_some()
        {
            return Ok(());
        }
        self.mutate_graph(
            |graph| graph.delete(doc_id),
            |connection| {
                connection.execute(
                    "DELETE FROM _vectors WHERE table_name = ?1 AND field = ?2 AND doc_id = ?3",
                    params![self.persistent.table, self.persistent.field, encoded],
                )?;
                Ok(())
            },
        )
    }

    pub(super) fn clear_graph(&self) -> StorageBackendResult<()> {
        if self
            .persistent
            .write_native(|read, batch| {
                self.mutate_native(read, batch, HNSWIndex::clear, |read, batch| {
                    read.clear_family(batch, crate::mvcc::native::NativeRecordFamily::Vectors)
                })
            })?
            .is_some()
        {
            return Ok(());
        }
        self.mutate_graph(HNSWIndex::clear, |connection| {
            connection.execute(
                "DELETE FROM _vectors WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            Ok(())
        })
    }

    fn mutate_graph(
        &self,
        mutate: impl FnOnce(&mut HNSWIndex) -> StorageBackendResult<()>,
        write_canonical: impl FnOnce(&Connection) -> Result<()>,
    ) -> StorageBackendResult<()> {
        let (candidate, identity) =
            self.persistent.conn.with_snapshot(|connection, identity| {
                let Some((_, _, _, revision)) = load_meta_from(connection, self)? else {
                    if self.require_persisted_graph {
                        return Err(missing_metadata(self).into());
                    }
                    write_canonical(connection)?;
                    return Ok(None);
                };
                let cached = self.cached_graph_at(connection, identity, revision)?;
                let mut graph = cached.graph.as_ref().clone();
                mutate(&mut graph)?;
                let delta = graph.take_persistence_delta();
                let next = next_revision(Some(revision))?;
                write_canonical(connection)?;
                self.persist_delta(connection, &delta, Some(revision), next)?;
                Ok(Some((graph, next)))
            })?;
        if let Some((graph, revision)) = candidate {
            self.publish_graph(graph, revision, identity);
        }
        Ok(())
    }
}

fn replace_canonical(
    connection: &Connection,
    index: &SQLiteHNSWIndex,
    doc_id: i64,
    vectors: &[EncodedVector],
) -> Result<()> {
    connection.execute(
        "DELETE FROM _vectors WHERE table_name = ?1 AND field = ?2 AND doc_id = ?3",
        params![index.persistent.table, index.persistent.field, doc_id],
    )?;
    let mut insert = connection.prepare("INSERT INTO _vectors (table_name, field, doc_id, vector_ordinal, vector) VALUES (?1, ?2, ?3, ?4, ?5)")?;
    for (ordinal, vector) in vectors {
        insert.execute(params![
            index.persistent.table,
            index.persistent.field,
            doc_id,
            ordinal,
            vector
        ])?;
    }
    Ok(())
}

pub(super) fn missing_metadata(index: &SQLiteHNSWIndex) -> StorageBackendError {
    StorageBackendError::Other(format!(
        "missing persisted HNSW metadata for {}.{}",
        index.persistent.table, index.persistent.field
    ))
}

pub(super) fn next_revision(current: Option<u64>) -> StorageBackendResult<u64> {
    current
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| StorageBackendError::Other("HNSW metadata revision space exhausted".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Catalog, ManagedConnection, SQLiteVectorIndex};
    use uqa_storage::vector_index::HNSWIndexParams;

    #[test]
    fn graph_evaluation_does_not_publish_over_an_intervening_same_revision_rebuild() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("evaluation.db");
        let first = ManagedConnection::open(&path).unwrap();
        Catalog::open(first.clone()).unwrap();
        let mut index = SQLiteHNSWIndex::new(first.clone(), "docs", "embedding", 2);
        index.add(1, vec![1.0, 0.0]).unwrap();
        index.initialize().unwrap();
        let revision = index.persisted_revision().unwrap();
        let second = ManagedConnection::open(&path).unwrap();
        let mut calls = 0;
        let result = index.mutate_graph(
            |candidate| {
                calls += 1;
                second.begin_transaction().unwrap();
                SQLiteHNSWIndex::drop_metadata(&second, "docs", "embedding").unwrap();
                let mut raw = SQLiteVectorIndex::new(second.clone(), "docs", "embedding", 2);
                raw.clear().unwrap();
                raw.add(4, vec![-1.0, 0.0]).unwrap();
                let mut rebuilt = SQLiteHNSWIndex::with_params(
                    second.clone(),
                    "docs",
                    "embedding",
                    2,
                    HNSWIndexParams::default(),
                );
                rebuilt.initialize().unwrap();
                assert_eq!(rebuilt.persisted_revision().unwrap(), revision);
                second.commit_transaction().unwrap();
                candidate.add(3, vec![0.0, 1.0])
            },
            |connection| {
                replace_canonical(
                    connection,
                    &index,
                    3,
                    &[(0, crate::vector_index::vector_to_blob(&[0.0, 1.0])?)],
                )
            },
        );
        let StorageBackendError::Backend { source, .. } = result.unwrap_err() else {
            panic!("expected a typed SQLite snapshot conflict");
        };
        let crate::SQLiteError::SQLite(rusqlite::Error::SqliteFailure(code, _)) =
            source.downcast_ref::<crate::SQLiteError>().unwrap()
        else {
            panic!("expected a SQLite failure code");
        };
        assert_eq!(code.extended_code, rusqlite::ffi::SQLITE_BUSY_SNAPSHOT);
        assert_eq!(calls, 1);
        assert!(!first.in_transaction());
        assert_eq!(index.count().unwrap(), 1);
        assert_eq!(
            index
                .search_knn(&[-1.0, 0.0], 1)
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>(),
            vec![4]
        );
        drop((index, first, second));
        let reopened = SQLiteHNSWIndex::open_existing(
            ManagedConnection::open(&path).unwrap(),
            "docs",
            "embedding",
            2,
            HNSWIndexParams::default(),
        );
        assert_eq!(reopened.count().unwrap(), 1);
        assert_eq!(
            reopened
                .search_knn(&[-1.0, 0.0], 1)
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>(),
            vec![4]
        );
    }
}
