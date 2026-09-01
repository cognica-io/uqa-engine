//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transactional IVF metadata replacement and removal.

use rusqlite::params;

use super::metadata::{state_to_str, usize_to_i64, EncodedIVFMetadata};
use crate::ivf_index::IVFState;
use crate::sqlite::vector_index::SQLiteVectorIndex;
use crate::sqlite::{ManagedConnection, Result as SQLiteResult};
use crate::vector_index::IVFIndexParams;

pub(super) fn write_metadata(
    connection: &rusqlite::Connection,
    persistent: &SQLiteVectorIndex,
    metadata: &EncodedIVFMetadata,
) -> SQLiteResult<()> {
    write_meta_values(
        connection,
        persistent,
        metadata.nlist,
        metadata.nprobe,
        metadata.train_threshold,
        metadata.state,
        metadata.trained_size,
        metadata.deletes_since_train,
        metadata.vector_count,
    )?;
    connection.execute(
        "DELETE FROM _ivf_centroids WHERE table_name = ?1 AND field = ?2",
        params![persistent.table, persistent.field],
    )?;
    connection.execute(
        "DELETE FROM _ivf_assignments WHERE table_name = ?1 AND field = ?2",
        params![persistent.table, persistent.field],
    )?;
    let mut centroid_insert = connection.prepare(
        "INSERT INTO _ivf_centroids (table_name, field, centroid_id, vector)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (centroid_id, centroid) in &metadata.centroids {
        centroid_insert.execute(params![
            persistent.table,
            persistent.field,
            centroid_id,
            centroid
        ])?;
    }
    let mut assignment_insert = connection.prepare(
        "INSERT INTO _ivf_assignments
            (table_name, field, doc_id, vector_ordinal, centroid_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (doc_id, ordinal, centroid) in &metadata.assignments {
        assignment_insert.execute(params![
            persistent.table,
            persistent.field,
            doc_id,
            ordinal,
            centroid
        ])?;
    }
    Ok(())
}

pub(super) fn write_untrained_meta(
    connection: &rusqlite::Connection,
    persistent: &SQLiteVectorIndex,
    params: IVFIndexParams,
    vector_count: usize,
) -> SQLiteResult<()> {
    write_meta_values(
        connection,
        persistent,
        usize_to_i64("nlist", params.nlist)?,
        usize_to_i64("nprobe", params.nprobe)?,
        usize_to_i64("train_threshold", params.train_threshold)?,
        IVFState::Untrained,
        0,
        0,
        usize_to_i64("vector_count", vector_count)?,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps persisted write inputs aligned"
)]
fn write_meta_values(
    connection: &rusqlite::Connection,
    persistent: &SQLiteVectorIndex,
    nlist: i64,
    nprobe: i64,
    train_threshold: i64,
    state: IVFState,
    trained_size: i64,
    deletes_since_train: i64,
    vector_count: i64,
) -> SQLiteResult<()> {
    connection.execute(
        "INSERT OR REPLACE INTO _ivf_indexes
            (table_name, field, dimensions, nlist, nprobe, train_threshold,
             state, trained_size, deletes_since_train, vector_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            persistent.table,
            persistent.field,
            i64::from(persistent.dimensions),
            nlist,
            nprobe,
            train_threshold,
            state_to_str(state),
            trained_size,
            deletes_since_train,
            vector_count,
        ],
    )?;
    Ok(())
}

pub(super) fn drop_metadata(
    connection: &ManagedConnection,
    table: &str,
    field: &str,
) -> SQLiteResult<()> {
    connection.with_mut(|connection| {
        let transaction = connection.savepoint()?;
        for metadata_table in ["_ivf_assignments", "_ivf_centroids", "_ivf_indexes"] {
            transaction.execute(
                &format!("DELETE FROM {metadata_table} WHERE table_name = ?1 AND field = ?2"),
                params![table, field],
            )?;
        }
        transaction.commit()?;
        Ok(())
    })
}
