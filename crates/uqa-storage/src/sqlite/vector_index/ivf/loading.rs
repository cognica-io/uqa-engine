//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! IVF catalog metadata and candidate-list loading.

use rusqlite::{params, OptionalExtension};
use uqa_core::DocId;

use super::metadata::{
    invalid_metadata, parse_state, positive_i64_to_usize, usize_to_i64, SQLiteIVFMeta,
};
use super::SQLiteIVFIndex;
use crate::sqlite::vector_index::codec::{blob_to_vector, decode_doc_id, i64_to_usize};
use crate::sqlite::Result as SQLiteResult;
use crate::vector_index::IVFIndexParams;

impl SQLiteIVFIndex {
    pub(super) fn load_meta(&self) -> SQLiteResult<Option<SQLiteIVFMeta>> {
        let row = self.persistent.conn.with(|conn| {
            Ok(conn
                .query_row(
                    "SELECT dimensions, nlist, nprobe, train_threshold, state,
                        trained_size, deletes_since_train, vector_count
                   FROM _ivf_indexes
                  WHERE table_name = ?1 AND field = ?2",
                    params![self.persistent.table, self.persistent.field],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, i64>(6)?,
                            row.get::<_, i64>(7)?,
                        ))
                    },
                )
                .optional()?)
        })?;
        let Some((dimensions, nlist, nprobe, threshold, state, trained, deleted, count)) = row
        else {
            return Ok(None);
        };
        i64_to_usize("IVF trained_size", trained)?;
        i64_to_usize("IVF deletes_since_train", deleted)?;
        Ok(Some(SQLiteIVFMeta {
            dimensions: u32::try_from(dimensions)
                .map_err(|_| invalid_metadata("dimensions", dimensions))?,
            params: IVFIndexParams {
                nlist: positive_i64_to_usize("nlist", nlist)?,
                nprobe: positive_i64_to_usize("nprobe", nprobe)?,
                train_threshold: positive_i64_to_usize("train_threshold", threshold)?,
            },
            state: parse_state(&state)?,
            vector_count: i64_to_usize("IVF vector_count", count)?,
        }))
    }

    pub(super) fn load_centroids(&self) -> SQLiteResult<Vec<Vec<f32>>> {
        self.persistent.conn.with(|conn| {
            let mut statement = conn.prepare(
                "SELECT vector FROM _ivf_centroids
                  WHERE table_name = ?1 AND field = ?2
                  ORDER BY centroid_id",
            )?;
            let rows = statement.query_map(
                params![self.persistent.table, self.persistent.field],
                |row| row.get::<_, Vec<u8>>(0),
            )?;
            let mut centroids = Vec::new();
            for row in rows {
                let centroid = blob_to_vector(&row?)?;
                self.persistent.validate_dimensions_sqlite(&centroid)?;
                centroids.push(centroid);
            }
            Ok(centroids)
        })
    }

    pub(super) fn load_candidates(
        &self,
        centroids: &[usize],
    ) -> SQLiteResult<Vec<(DocId, Vec<f32>)>> {
        self.persistent.conn.with(|conn| {
            let mut output = Vec::new();
            let mut statement = conn.prepare(
                "SELECT v.doc_id, v.vector
                   FROM _ivf_assignments a
                   JOIN _vectors v
                     ON v.table_name = a.table_name AND v.field = a.field
                    AND v.doc_id = a.doc_id AND v.vector_ordinal = a.vector_ordinal
                  WHERE a.table_name = ?1 AND a.field = ?2 AND a.centroid_id = ?3
                  ORDER BY v.doc_id, v.vector_ordinal",
            )?;
            for centroid in centroids {
                let rows = statement.query_map(
                    params![
                        self.persistent.table,
                        self.persistent.field,
                        usize_to_i64("centroid_id", *centroid)?
                    ],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )?;
                for row in rows {
                    let (doc_id, blob) = row?;
                    let vector = blob_to_vector(&blob)?;
                    self.persistent.validate_dimensions_sqlite(&vector)?;
                    output.push((decode_doc_id(doc_id)?, vector));
                }
            }
            Ok(output)
        })
    }
}
