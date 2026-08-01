//! Persistent IVF metadata representation and transactional encoding.

use super::{
    encode_doc_id, params, vector_to_blob, IVFMetadataSnapshot, IVFState, SQLiteError,
    SQLiteResult, SQLiteVectorIndex,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SQLiteIVFParams {
    pub(super) nlist: usize,
    pub(super) nprobe: usize,
    pub(super) train_threshold: usize,
}

impl SQLiteIVFParams {
    pub(super) fn new(nlist: usize, nprobe: usize, train_threshold: usize) -> Self {
        let nlist = nlist.max(1);
        Self {
            nlist,
            nprobe: nprobe.max(1),
            train_threshold: train_threshold.max(1),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct SQLiteIVFMeta {
    pub(super) dimensions: u32,
    pub(super) params: SQLiteIVFParams,
    pub(super) state: IVFState,
    pub(super) vector_count: usize,
}

pub(super) struct EncodedIVFMetadata {
    pub(super) nlist: i64,
    pub(super) nprobe: i64,
    pub(super) train_threshold: i64,
    pub(super) state: IVFState,
    pub(super) trained_size: i64,
    pub(super) deletes_since_train: i64,
    pub(super) vector_count: i64,
    pub(super) centroids: Vec<(i64, Vec<u8>)>,
    pub(super) assignments: Vec<(i64, i64, i64)>,
}

pub(super) fn invalid_ivf_metadata(field: &str, value: i64) -> SQLiteError {
    SQLiteError::StorageBackend(format!("invalid IVF metadata {field}: {value}"))
}

pub(super) fn i64_to_usize(field: &str, value: i64) -> SQLiteResult<usize> {
    value
        .try_into()
        .map_err(|_| invalid_ivf_metadata(field, value))
}

pub(super) fn positive_i64_to_usize(field: &str, value: i64) -> SQLiteResult<usize> {
    let value = i64_to_usize(field, value)?;
    if value == 0 {
        Err(SQLiteError::StorageBackend(format!(
            "invalid IVF metadata {field}: expected a positive value"
        )))
    } else {
        Ok(value)
    }
}

pub(super) fn usize_to_i64(field: &str, value: usize) -> SQLiteResult<i64> {
    value.try_into().map_err(|_| {
        SQLiteError::StorageBackend(format!(
            "IVF metadata {field} does not fit in SQLite INTEGER"
        ))
    })
}

pub(super) fn encode_ivf_metadata(
    params_value: SQLiteIVFParams,
    snapshot: &IVFMetadataSnapshot,
) -> SQLiteResult<EncodedIVFMetadata> {
    let centroids = snapshot
        .centroids
        .iter()
        .enumerate()
        .map(|(centroid_id, centroid)| {
            Ok((
                usize_to_i64("centroid_id", centroid_id)?,
                vector_to_blob(centroid)?,
            ))
        })
        .collect::<SQLiteResult<Vec<_>>>()?;
    let assignments = snapshot
        .assignments
        .iter()
        .map(|(doc_id, vector_ordinal, centroid_id)| {
            Ok((
                encode_doc_id(*doc_id)?,
                i64::from(*vector_ordinal),
                usize_to_i64("centroid_id", *centroid_id)?,
            ))
        })
        .collect::<SQLiteResult<Vec<_>>>()?;
    Ok(EncodedIVFMetadata {
        nlist: usize_to_i64("nlist", params_value.nlist)?,
        nprobe: usize_to_i64("nprobe", params_value.nprobe)?,
        train_threshold: usize_to_i64("train_threshold", params_value.train_threshold)?,
        state: snapshot.state,
        trained_size: usize_to_i64("trained_size", snapshot.trained_size)?,
        deletes_since_train: usize_to_i64("deletes_since_train", snapshot.deletes_since_train)?,
        vector_count: usize_to_i64("vector_count", snapshot.vector_count)?,
        centroids,
        assignments,
    })
}

pub(super) fn write_encoded_metadata(
    conn: &rusqlite::Connection,
    persistent: &SQLiteVectorIndex,
    metadata: &EncodedIVFMetadata,
) -> SQLiteResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO _ivf_indexes
            (table_name, field, dimensions, nlist, nprobe, train_threshold,
             state, trained_size, deletes_since_train, vector_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            persistent.table,
            persistent.field,
            i64::from(persistent.dimensions),
            metadata.nlist,
            metadata.nprobe,
            metadata.train_threshold,
            state_to_str(metadata.state),
            metadata.trained_size,
            metadata.deletes_since_train,
            metadata.vector_count,
        ],
    )?;
    conn.execute(
        "DELETE FROM _ivf_centroids WHERE table_name = ?1 AND field = ?2",
        params![persistent.table, persistent.field],
    )?;
    conn.execute(
        "DELETE FROM _ivf_assignments WHERE table_name = ?1 AND field = ?2",
        params![persistent.table, persistent.field],
    )?;
    {
        let mut statement = conn.prepare(
            "INSERT INTO _ivf_centroids
                (table_name, field, centroid_id, vector)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (centroid_id, centroid) in &metadata.centroids {
            statement.execute(params![
                persistent.table,
                persistent.field,
                centroid_id,
                centroid,
            ])?;
        }
    }
    {
        let mut statement = conn.prepare(
            "INSERT INTO _ivf_assignments
                (table_name, field, doc_id, vector_ordinal, centroid_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (doc_id, ordinal, centroid_id) in &metadata.assignments {
            statement.execute(params![
                persistent.table,
                persistent.field,
                doc_id,
                ordinal,
                centroid_id,
            ])?;
        }
    }
    Ok(())
}

pub(super) fn write_meta_row(
    conn: &rusqlite::Connection,
    persistent: &SQLiteVectorIndex,
    params_value: SQLiteIVFParams,
    state: IVFState,
    trained_size: usize,
    deletes_since_train: usize,
    vector_count: usize,
) -> SQLiteResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO _ivf_indexes
            (table_name, field, dimensions, nlist, nprobe, train_threshold,
             state, trained_size, deletes_since_train, vector_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            persistent.table,
            persistent.field,
            i64::from(persistent.dimensions),
            usize_to_i64("nlist", params_value.nlist)?,
            usize_to_i64("nprobe", params_value.nprobe)?,
            usize_to_i64("train_threshold", params_value.train_threshold)?,
            state_to_str(state),
            usize_to_i64("trained_size", trained_size)?,
            usize_to_i64("deletes_since_train", deletes_since_train)?,
            usize_to_i64("vector_count", vector_count)?,
        ],
    )?;
    Ok(())
}

pub(super) fn state_to_str(state: IVFState) -> &'static str {
    match state {
        IVFState::Untrained => "untrained",
        IVFState::Trained => "trained",
        IVFState::Stale => "stale",
    }
}

pub(super) fn str_to_state(value: &str) -> SQLiteResult<IVFState> {
    match value {
        "untrained" => Ok(IVFState::Untrained),
        "trained" => Ok(IVFState::Trained),
        "stale" => Ok(IVFState::Stale),
        other => Err(SQLiteError::StorageBackend(format!(
            "invalid IVF metadata state: {other}"
        ))),
    }
}
