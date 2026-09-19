//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Convert field-named accelerator tables into unambiguous table-owned record families.

use super::{invalid, Family, PhysicalResult};
use rusqlite::{params, Connection, OptionalExtension};
use uqa_core::memory::{BudgetedVec, MemoryError};
use uqa_storage::{mvcc::VersionError, read_control::StorageReadControl};

pub(super) const TABLES: [(Family, &str); 2] = [
    (Family::OccurrenceSkips, "CREATE TABLE _occurrence_skips (table_name TEXT NOT NULL, field TEXT NOT NULL, term BLOB NOT NULL, skip_doc_id INTEGER NOT NULL CHECK(skip_doc_id >= 0), skip_offset INTEGER NOT NULL CHECK(skip_offset >= 0), PRIMARY KEY(table_name, field, term, skip_doc_id)) WITHOUT ROWID"),
    (Family::OccurrenceBlockMax, "CREATE TABLE _occurrence_block_max (table_name TEXT NOT NULL, field TEXT NOT NULL, term BLOB NOT NULL, block_idx INTEGER NOT NULL CHECK(block_idx >= 0), max_score REAL NOT NULL CHECK(max_score >= 0 AND max_score <= 1.7976931348623157e308), scorer_fingerprint TEXT NOT NULL, PRIMARY KEY(table_name, field, term, block_idx)) WITHOUT ROWID"),
];

pub(super) fn create(connection: &Connection) -> PhysicalResult<()> {
    for (family, sql) in TABLES {
        connection.execute_batch(sql)?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            connection.execute_batch(
                &crate::Catalog::cache_revision_trigger(family.layout().table, true, action).1,
            )?;
        }
    }
    Ok(())
}

pub(super) fn import(connection: &Connection, control: &StorageReadControl) -> PhysicalResult<()> {
    let mut after = BudgetedVec::new(control.memory());
    let suffix = "FROM sqlite_schema WHERE type = 'table' AND (name GLOB '_skip_*' OR name GLOB '_blockmax_*') AND name > ?1 ORDER BY name LIMIT 1";
    loop {
        control.check().map_err(VersionError::from)?;
        let previous = std::str::from_utf8(&after).expect("retained SQLite name");
        let length: Option<i64> = connection
            .query_row(
                &format!("SELECT octet_length(name) {suffix}"),
                [previous],
                |row| row.get(0),
            )
            .optional()?;
        let Some(length) = length else {
            break;
        };
        let bytes = usize::try_from(length)
            .map_err(|_| invalid("invalid accelerator name size"))?
            .checked_mul(24)
            .and_then(|size| size.checked_add(1024))
            .ok_or(VersionError::from(MemoryError::SizeOverflow))?;
        let _names_and_sql = control
            .memory()
            .reserve(bytes)
            .map_err(VersionError::from)?;
        let name: String =
            connection.query_row(&format!("SELECT name {suffix}"), [previous], |row| {
                row.get(0)
            })?;
        let quoted = format!("\"{}\"", name.replace('"', "\"\""));
        let skip = name.starts_with("_skip_");
        let fingerprint = validate_source(connection, &quoted, skip)?;
        let populated: bool = connection.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {quoted})"),
            [],
            |row| row.get(0),
        )?;
        if populated {
            let (table, field) = owner(
                connection,
                &name,
                if skip { "_skip_" } else { "_blockmax_" },
                control,
            )?;
            let table = std::str::from_utf8(&table).expect("source table name");
            let field = std::str::from_utf8(&field).expect("source field name");
            let sql = if skip {
                format!("INSERT INTO _occurrence_skips SELECT ?1, ?2, term, skip_doc_id, skip_offset FROM {quoted}")
            } else {
                let fingerprint = if fingerprint {
                    "scorer_fingerprint"
                } else {
                    "''"
                };
                format!("INSERT INTO _occurrence_block_max SELECT ?1, ?2, term, block_idx, max_score, {fingerprint} FROM {quoted}")
            };
            // SQLite transfers rows directly inside this conversion transaction; no corpus-sized Rust payload collection is retained.
            connection.execute(&sql, params![table, field])?;
        }
        // Empty orphan accelerators carry no state. Populated tables require an exact, unique source-field owner before retirement.
        connection.execute_batch(&format!("DROP TABLE {quoted}"))?;
        after.clear();
        after
            .extend_from_slice(name.as_bytes())
            .map_err(VersionError::from)?;
    }
    Ok(())
}

fn validate_source(connection: &Connection, table: &str, skip: bool) -> PhysicalResult<bool> {
    let expected: &[(&str, &str, u16)] = if skip {
        &[
            ("term", "BLOB", 1),
            ("skip_doc_id", "INTEGER", 2),
            ("skip_offset", "INTEGER", 0),
        ]
    } else {
        &[
            ("term", "BLOB", 1),
            ("block_idx", "INTEGER", 2),
            ("max_score", "REAL", 0),
            ("scorer_fingerprint", "TEXT", 0),
        ]
    };
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = statement.query([])?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        let Some(&(name, kind, primary)) = expected.get(count) else {
            return Err(invalid("unexpected occurrence accelerator column").into());
        };
        if row.get_ref(1)?.as_str().ok() != Some(name)
            || row.get_ref(2)?.as_str().ok() != Some(kind)
            || !row.get::<_, bool>(3)?
            || row.get::<_, u16>(5)? != primary
        {
            return Err(invalid("invalid occurrence accelerator source layout").into());
        }
        count += 1;
    }
    if count != expected.len() && (skip || count != 3) {
        return Err(invalid("missing occurrence accelerator source column").into());
    }
    Ok(count == 4)
}

fn owner(
    connection: &Connection,
    name: &str,
    prefix: &str,
    control: &StorageReadControl,
) -> PhysicalResult<(BudgetedVec<u8>, BudgetedVec<u8>)> {
    let mut statement = connection.prepare("WITH fields(table_name, field) AS (SELECT table_name, field FROM _occurrence_fields UNION SELECT table_name, field FROM _occurrence_lengths UNION SELECT table_name, field FROM _field_stats UNION SELECT table_name, field FROM _doc_lengths UNION SELECT table_name, field FROM _table_field_analyzers) SELECT table_name, field FROM fields WHERE ?1 = ?2 || table_name || '_' || field LIMIT 2")?;
    let mut rows = statement.query(params![name, prefix])?;
    let row = rows
        .next()?
        .ok_or_else(|| invalid("populated occurrence accelerator has no source-field owner"))?;
    let mut table = BudgetedVec::new(control.memory());
    let mut field = BudgetedVec::new(control.memory());
    table
        .extend_from_slice(
            row.get_ref(0)?
                .as_str()
                .map_err(|_| invalid("invalid accelerator table name"))?
                .as_bytes(),
        )
        .map_err(VersionError::from)?;
    field
        .extend_from_slice(
            row.get_ref(1)?
                .as_str()
                .map_err(|_| invalid("invalid accelerator field name"))?
                .as_bytes(),
        )
        .map_err(VersionError::from)?;
    if rows.next()?.is_some() {
        return Err(invalid(
            "populated occurrence accelerator has ambiguous source-field ownership",
        )
        .into());
    }
    Ok((table, field))
}
