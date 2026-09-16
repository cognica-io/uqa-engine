//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared decoding for legacy SQL rows and native sequence records.

use super::{RelationIdentity, Result, SQLiteError, SequenceOptions, SequenceRow};
use uqa_storage::catalog::{SequenceOwner, SequenceOwnerDependency};

pub(in crate::catalog) fn concrete_sequence_options(sequence: &SequenceRow) -> SequenceOptions {
    let default_min = if sequence.increment > 0 { 1 } else { i64::MIN };
    let default_max = if sequence.increment > 0 { i64::MAX } else { -1 };
    SequenceOptions {
        data_type: sequence.options.data_type.clone(),
        min_value: Some(sequence.options.min_value.unwrap_or(default_min)),
        max_value: Some(sequence.options.max_value.unwrap_or(default_max)),
        cycle: sequence.options.cycle,
        cache_size: sequence.options.cache_size,
    }
}

fn decode_sequence_owner(
    relation: &RelationIdentity,
    table_object_id: Option<Vec<u8>>,
    column_object_id: Option<Vec<u8>>,
    dependency: Option<String>,
) -> Result<Option<SequenceOwner>> {
    let (table_object_id, column_object_id, dependency) =
        match (table_object_id, column_object_id, dependency) {
            (None, None, None) => return Ok(None),
            (Some(table), Some(column), Some(dependency)) => (table, column, dependency),
            _ => {
                return Err(SQLiteError::StorageBackend(format!(
                    "corrupt sequence `{}` has an incomplete owner dependency",
                    relation.qualified_name()
                )))
            }
        };
    let table_object_id: [u8; 16] = table_object_id.try_into().map_err(|value: Vec<u8>| {
        SQLiteError::StorageBackend(format!(
            "corrupt sequence `{}` owner table identity has {} bytes",
            relation.qualified_name(),
            value.len()
        ))
    })?;
    let column_object_id: [u8; 16] = column_object_id.try_into().map_err(|value: Vec<u8>| {
        SQLiteError::StorageBackend(format!(
            "corrupt sequence `{}` owner column identity has {} bytes",
            relation.qualified_name(),
            value.len()
        ))
    })?;
    let dependency = match dependency.as_str() {
        "a" => SequenceOwnerDependency::Automatic,
        "i" => SequenceOwnerDependency::Internal,
        other => {
            return Err(SQLiteError::StorageBackend(format!(
                "corrupt sequence `{}` owner dependency `{other}`",
                relation.qualified_name()
            )))
        }
    };
    Ok(Some(SequenceOwner {
        table_object_id,
        column_object_id,
        dependency,
    }))
}

pub(super) struct RawSequenceRow {
    schema: String,
    name: String,
    object_id: Vec<u8>,
    definition_generation: Vec<u8>,
    start: i64,
    increment: i64,
    current: i64,
    called: bool,
    persistence: String,
    data_type: String,
    min_value: i64,
    max_value: i64,
    cycle: bool,
    cache_size: i64,
    owner_table_object_id: Option<Vec<u8>>,
    owner_column_object_id: Option<Vec<u8>>,
    owner_dependency: Option<String>,
    role_owner: String,
    acl_json: Option<String>,
    log_count: i64,
}

pub(super) fn read_raw_sequence_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawSequenceRow> {
    Ok(RawSequenceRow {
        schema: row.get(0)?,
        name: row.get(1)?,
        object_id: row.get(2)?,
        definition_generation: row.get(3)?,
        start: row.get(4)?,
        increment: row.get(5)?,
        current: row.get(6)?,
        called: row.get(7)?,
        persistence: row.get(8)?,
        data_type: row.get(9)?,
        min_value: row.get(10)?,
        max_value: row.get(11)?,
        cycle: row.get(12)?,
        cache_size: row.get(13)?,
        owner_table_object_id: row.get(14)?,
        owner_column_object_id: row.get(15)?,
        owner_dependency: row.get(16)?,
        role_owner: row.get(17)?,
        acl_json: row.get(18)?,
        log_count: row.get(19)?,
    })
}

pub(super) fn decode_sequence_identity(
    relation: &RelationIdentity,
    label: &str,
    value: Vec<u8>,
) -> Result<[u8; 16]> {
    value.try_into().map_err(|value: Vec<u8>| {
        SQLiteError::StorageBackend(format!(
            "corrupt sequence `{}` {label} has {} bytes",
            relation.qualified_name(),
            value.len()
        ))
    })
}

pub(super) fn decode_raw_sequence_row(raw: RawSequenceRow) -> Result<SequenceRow> {
    let relation = RelationIdentity::new(raw.schema, raw.name);
    Ok(SequenceRow {
        role_owner: raw.role_owner,
        acl: raw
            .acl_json
            .map(|json| serde_json::from_str(&json))
            .transpose()?,
        owner: decode_sequence_owner(
            &relation,
            raw.owner_table_object_id,
            raw.owner_column_object_id,
            raw.owner_dependency,
        )?,
        object_id: decode_sequence_identity(&relation, "object identity", raw.object_id)?,
        definition_generation: decode_sequence_identity(
            &relation,
            "definition generation",
            raw.definition_generation,
        )?,
        relation,
        start: raw.start,
        increment: raw.increment,
        current: raw.current,
        called: raw.called,
        log_count: raw.log_count,
        persistence: raw.persistence,
        options: SequenceOptions {
            data_type: raw.data_type,
            min_value: Some(raw.min_value),
            max_value: Some(raw.max_value),
            cycle: raw.cycle,
            cache_size: raw.cache_size,
        },
    })
}

pub(in crate::catalog) fn decode_native_sequence_row(
    row: &[rusqlite::types::ValueRef<'_>],
) -> Result<SequenceRow> {
    fn field<T: rusqlite::types::FromSql>(
        row: &[rusqlite::types::ValueRef<'_>],
        index: usize,
    ) -> Result<T> {
        T::column_result(row[index]).map_err(|error| {
            SQLiteError::StorageBackend(format!("invalid native sequence column {index}: {error}"))
        })
    }
    decode_raw_sequence_row(RawSequenceRow {
        schema: field(row, 0)?,
        name: field(row, 1)?,
        object_id: field(row, 8)?,
        definition_generation: field(row, 14)?,
        start: field(row, 3)?,
        increment: field(row, 4)?,
        current: field(row, 5)?,
        called: field(row, 6)?,
        persistence: field(row, 7)?,
        data_type: field(row, 9)?,
        min_value: field(row, 10)?,
        max_value: field(row, 11)?,
        cycle: field(row, 12)?,
        cache_size: field(row, 13)?,
        owner_table_object_id: field(row, 15)?,
        owner_column_object_id: field(row, 16)?,
        owner_dependency: field(row, 17)?,
        role_owner: field(row, 18)?,
        acl_json: field(row, 19)?,
        log_count: field(row, 20)?,
    })
}
