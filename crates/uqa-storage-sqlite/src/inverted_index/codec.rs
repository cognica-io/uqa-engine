//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite` integer bounds, posting-position codecs, and aggregate loaders.

use super::{params, BTreeMap, FieldName, OptionalExtension, SQLiteError, SQLiteResult};

/// A score materialization is valid only for the exact posting/statistics
/// snapshot it was built from. Clear every field-local block-max table in the
/// same transaction as a posting mutation; stale bounds could otherwise make
/// an exact top-k query return the wrong documents.
pub(super) fn invalidate_block_max_tables(
    conn: &rusqlite::Connection,
    logical_table: &str,
) -> SQLiteResult<()> {
    let prefix = format!("_blockmax_{logical_table}_");
    let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for name in names.into_iter().filter(|name| name.starts_with(&prefix)) {
        conn.execute(&format!("DELETE FROM {}", quote_ident(&name)), [])?;
    }
    Ok(())
}

pub(super) fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub(super) fn encode_index_u64(kind: &str, value: u64) -> SQLiteResult<i64> {
    i64::try_from(value).map_err(|_| {
        SQLiteError::StorageBackend(format!(
            "{kind} id {value} exceeds the SQLite INTEGER range"
        ))
    })
}

pub(super) fn encode_index_usize(kind: &str, value: usize) -> SQLiteResult<i64> {
    i64::try_from(value).map_err(|_| {
        SQLiteError::StorageBackend(format!("{kind} {value} exceeds the SQLite INTEGER range"))
    })
}

pub(super) fn usize_to_index_u64(kind: &str, value: usize) -> SQLiteResult<u64> {
    u64::try_from(value).map_err(|_| {
        SQLiteError::StorageBackend(format!("{kind} {value} exceeds the u64 counter range"))
    })
}

pub(super) fn encode_index_counter(kind: &str, value: u64) -> SQLiteResult<i64> {
    i64::try_from(value).map_err(|_| {
        SQLiteError::StorageBackend(format!("{kind} {value} exceeds the SQLite INTEGER range"))
    })
}

pub(super) fn validate_position_count(token_count: u64) -> SQLiteResult<()> {
    if token_count > u64::from(u32::MAX) + 1 {
        return Err(SQLiteError::StorageBackend(
            "token positions exceed the u32 index format".into(),
        ));
    }
    Ok(())
}

pub(super) fn corrupt_counter(message: &str) -> SQLiteError {
    SQLiteError::StorageBackend(format!("corrupt inverted index: {message}"))
}

pub(super) fn load_document_lengths(
    conn: &rusqlite::Connection,
    table: &str,
    doc_id: i64,
) -> SQLiteResult<BTreeMap<FieldName, u64>> {
    let mut stmt = conn.prepare(
        "SELECT field, length FROM _doc_lengths
         WHERE table_name = ?1 AND doc_id = ?2",
    )?;
    let rows = stmt.query_map(params![table, doc_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut lengths = BTreeMap::new();
    for row in rows {
        let (field, length) = row?;
        lengths.insert(field, decode_index_u64("document length", length)?);
    }
    Ok(lengths)
}

pub(super) fn load_field_total(
    conn: &rusqlite::Connection,
    table: &str,
    field: &str,
) -> SQLiteResult<Option<u64>> {
    let total: Option<i64> = conn
        .query_row(
            "SELECT total_length FROM _field_stats
             WHERE table_name = ?1 AND field = ?2",
            params![table, field],
            |row| row.get(0),
        )
        .optional()?;
    total
        .map(|value| decode_index_u64("total field length", value))
        .transpose()
}

pub(super) fn decode_index_u64(kind: &str, value: i64) -> SQLiteResult<u64> {
    u64::try_from(value).map_err(|_| {
        SQLiteError::StorageBackend(format!("corrupt inverted index: negative {kind} {value}"))
    })
}

pub(super) fn decode_index_usize(kind: &str, value: i64) -> SQLiteResult<usize> {
    usize::try_from(value).map_err(|_| {
        SQLiteError::StorageBackend(format!("corrupt inverted index: invalid {kind} {value}"))
    })
}

pub(super) fn table_exists(conn: &rusqlite::Connection, name: &str) -> rusqlite::Result<bool> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
            [name],
            |row| row.get(0),
        )
        .optional()?;
    Ok(exists.is_some())
}
