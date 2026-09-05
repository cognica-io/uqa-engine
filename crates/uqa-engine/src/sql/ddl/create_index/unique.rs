//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unique-index build validation before catalog publication.

use super::{ddl_storage_error, CreateIndex, Engine, SQLError};
use uqa_core::Value;

pub(super) fn validate_unique_index(
    engine: &Engine,
    statement: &CreateIndex,
    name: &str,
) -> Result<(), SQLError> {
    if !statement.unique {
        return Ok(());
    }
    if !matches!(statement.access_method.as_str(), "" | "btree") {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!(
                "access method \"{}\" does not support unique indexes",
                statement.access_method
            ),
        });
    }
    let hierarchy = engine
        .try_table_hierarchy(&statement.table)
        .map_err(|error| ddl_storage_error("CREATE UNIQUE INDEX", error))?;
    if let Some(partition) = &hierarchy.partition_spec {
        for key in &partition.keys {
            if !matches!(key, uqa_sql::ast::Expr::Column(column) if statement.columns.iter().any(|key| key.column() == Some(column.as_str())))
            {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: "unique constraint on partitioned table must include all partitioning columns".into(),
                });
            }
        }
    }
    let tables = if hierarchy.partition_spec.is_some() {
        engine.hierarchy_scan_tables(&statement.table, true)?
    } else {
        vec![statement.table.clone()]
    };
    let directory = tempfile::tempdir().map_err(|error| SQLError::Internal(error.to_string()))?;
    let connection = rusqlite::Connection::open(directory.path().join("index-build.sqlite"))
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    connection
        .execute_batch("CREATE TABLE index_keys(key BLOB PRIMARY KEY) WITHOUT ROWID")
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    for table in tables {
        for id in engine.live_table_doc_ids(&table)? {
            let document = engine.get_document(&table, id)?.ok_or_else(|| {
                SQLError::Internal("unique-index build lost a visible row".into())
            })?;
            if !crate::sql::dml::index_predicate_accepts(
                engine,
                &table,
                statement.predicate.as_deref(),
                &document,
            )? {
                continue;
            }
            let values =
                crate::sql::dml::index_key_values(engine, &table, &statement.columns, &document)?;
            if !statement.nulls_not_distinct
                && values.iter().any(|value| matches!(value, Value::Null))
            {
                continue;
            }
            let key = uqa_execution::canonical_row_key(&values)
                .map_err(crate::sql::select::physical_exec_error)?;
            let inserted = connection
                .execute("INSERT OR IGNORE INTO index_keys VALUES(?1)", [key])
                .map_err(|error| SQLError::Internal(error.to_string()))?;
            if inserted == 0 {
                return Err(SQLError::Routine {
                    sqlstate: "23505".into(),
                    message: format!("could not create unique index \"{name}\": key is duplicated"),
                });
            }
        }
    }
    Ok(())
}
