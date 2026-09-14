//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed-size metadata and scalar format validation without vocabulary materialization.

use super::{
    blob, decode_index_u64, encode_index_u64, reserve_bindings, Connection, DocId, SQLiteError,
    SQLiteInvertedIndex, SQLiteResult, StorageReadControl,
};
use crate::inverted_index::{
    data::FieldStats,
    format::{FIELD_TABLES, LEGACY_TABLES},
};
use rusqlite::{params, OptionalExtension};
use uqa_core::memory::BudgetedString;
use uqa_storage::inverted_index::{IndexedFieldMetadata, IndexedFieldRevision};

impl SQLiteInvertedIndex {
    pub(super) fn require_graph_format_budgeted_on(
        &self,
        connection: &Connection,
        control: &StorageReadControl,
    ) -> SQLiteResult<()> {
        let _bindings = reserve_bindings(control, &[self.table.as_bytes()])?;
        let format: Option<i64> = connection.query_row(
            "SELECT CASE format WHEN 'occurrences-v2' THEN 1 WHEN 'source-rebuild' THEN 2 ELSE 0 END FROM _occurrence_formats WHERE table_name = ?1", [&self.table], |row| row.get(0),
        ).optional()?;
        control.check()?;
        match format {
            Some(1) | None => {}
            Some(2) => return Err(rebuild()),
            _ => {
                return Err(SQLiteError::StorageBackend(
                    "unsupported occurrence index format".into(),
                ))
            }
        }
        for table in LEGACY_TABLES {
            control.check()?;
            let _binding = reserve_bindings(control, &[table.as_bytes()])?;
            let exists: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [table],
                |row| row.get(0),
            )?;
            if exists && self.has_rows_budgeted_on(connection, table, control)? {
                return Err(rebuild());
            }
        }
        if format.is_none() {
            for table in FIELD_TABLES {
                if self.has_rows_budgeted_on(connection, table, control)? {
                    return Err(SQLiteError::StorageBackend(
                        "occurrence index format marker is missing".into(),
                    ));
                }
            }
        }
        control.check()?;
        Ok(())
    }

    fn has_rows_budgeted_on(
        &self,
        connection: &Connection,
        table: &str,
        control: &StorageReadControl,
    ) -> SQLiteResult<bool> {
        control.check()?;
        let mut sql = BudgetedString::new(control.memory());
        sql.reserve(
            "SELECT EXISTS(SELECT 1 FROM ".len() + table.len() + " WHERE table_name = ?1)".len(),
        )?;
        sql.push_str("SELECT EXISTS(SELECT 1 FROM ")?;
        sql.push_str(table)?;
        sql.push_str(" WHERE table_name = ?1)")?;
        // SQLite retains the prepared SQL text until the statement is finalized.
        let _sql = control.memory().reserve(sql.len())?;
        let found = connection.query_row(&sql, [&self.table], |row| row.get(0))?;
        control.check()?;
        Ok(found)
    }

    pub(super) fn field_stats_budgeted_on(
        &self,
        connection: &Connection,
        field: &str,
        control: &StorageReadControl,
    ) -> SQLiteResult<Option<FieldStats>> {
        let _bindings = reserve_bindings(control, &[self.table.as_bytes(), field.as_bytes()])?;
        let _payload = control.memory().reserve(40)?;
        let mut statement = connection.prepare("SELECT CASE WHEN typeof(revision) = 'blob' AND length(revision) = 40 THEN revision END, doc_count, total_length FROM _occurrence_fields WHERE table_name = ?1 AND field = ?2")?;
        let mut rows = statement.query(params![self.table, field])?;
        let Some(row) = rows.next()? else {
            control.check()?;
            return Ok(None);
        };
        control.check()?;
        let revision = IndexedFieldRevision::from_bytes(blob(row, 0)?)?;
        let doc_count = decode_index_u64("field document count", row.get(1)?)?;
        if doc_count == 0 {
            return Err(SQLiteError::StorageBackend(
                "empty field statistics must be removed".into(),
            ));
        }
        let total_length = decode_index_u64("total field length", row.get(2)?)?;
        Ok(Some(FieldStats {
            revision,
            doc_count,
            total_length,
        }))
    }

    pub(super) fn metadata_budgeted_on(
        &self,
        connection: &Connection,
        doc_id: DocId,
        field: &str,
        control: &StorageReadControl,
    ) -> SQLiteResult<IndexedFieldMetadata> {
        let metadata = {
            let _bindings = reserve_bindings(control, &[self.table.as_bytes(), field.as_bytes()])?;
            let _payload = control.memory().reserve(84)?;
            let mut statement = connection.prepare("SELECT CASE WHEN typeof(metadata_blob) = 'blob' AND length(metadata_blob) = 84 THEN metadata_blob END FROM _occurrence_documents WHERE table_name = ?1 AND doc_id = ?2 AND field = ?3")?;
            let mut rows = statement.query(params![
                self.table,
                encode_index_u64("document", doc_id)?,
                field
            ])?;
            let row = rows.next()?.ok_or_else(|| {
                SQLiteError::StorageBackend("occurrence source metadata is missing".into())
            })?;
            control.check()?;
            IndexedFieldMetadata::from_bytes(blob(row, 0)?)?
        };
        let stats = self
            .field_stats_budgeted_on(connection, field, control)?
            .ok_or_else(|| {
                SQLiteError::StorageBackend("indexed field revision is missing".into())
            })?;
        if stats.revision != metadata.revision() {
            return Err(SQLiteError::StorageBackend(
                "indexed field revisions disagree".into(),
            ));
        }
        control.check()?;
        Ok(metadata)
    }
}

fn rebuild() -> SQLiteError {
    SQLiteError::StorageBackend("legacy positional data requires an atomic source rebuild".into())
}
