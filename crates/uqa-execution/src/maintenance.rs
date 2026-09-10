//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute VACUUM target locking, physical rewrites, compaction and statistics refresh.
use std::collections::BTreeSet;
use uqa_sql::{ast::VacuumStmt, maintenance::ResolvedVacuumTarget, SQLError, SQLResult};
use uqa_storage::StorageBackendError;
mod context;
pub use context::*;
fn vacuum_storage_error(context: &str, error: impl std::fmt::Display) -> StorageBackendError {
    StorageBackendError::Other(format!("{context}: {error}"))
}

fn rewrite_full_vacuum_targets(
    context: &VacuumContext<'_>,
    targets: &[ResolvedVacuumTarget],
) -> Result<(), SQLError> {
    let mut tables = BTreeSet::new();
    for target in targets {
        tables.extend(
            context
                .relations
                .scan_tables(&target.table, target.include_descendants)?
                .into_iter(),
        );
    }
    for table in &tables {
        if let Err(error) = context.locks.lock_exclusive(table) {
            context.locks.release_session();
            return Err(SQLError::Internal(format!(
                "VACUUM FULL failed: lock relation: {error}"
            )));
        }
    }
    let result = context
        .transactions
        .with_maintenance_write(Box::new(|context| {
            for table in &tables {
                rewrite_full_vacuum_table(context, table)?;
            }
            Ok(())
        }))
        .and_then(|()| context.storage.vacuum())
        .map_err(|error| SQLError::Internal(format!("VACUUM FULL failed: {error}")));
    context.locks.release_session();
    result
}

fn rewrite_full_vacuum_table(
    context: &VacuumContext<'_>,
    table_name: &str,
) -> Result<(), StorageBackendError> {
    let table = context
        .relations
        .require_table(table_name)
        .map_err(|error| vacuum_storage_error("resolve VACUUM FULL relation", error))?;
    let stats = table.statistics();
    let documents = {
        let store = table.documents();
        let mut ids = store.doc_ids()?;
        ids.sort_unstable();
        let documents = store.get_stored_many(&ids)?;
        let mut rows = Vec::with_capacity(ids.len());
        for doc_id in ids {
            let document = documents.get(&doc_id).cloned().ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "VACUUM FULL relation `{table_name}` listed document {doc_id} but did not return it"
                ))
            })?;
            let vectors = table
                .document_vectors(document.fields())
                .map_err(|error| vacuum_storage_error("snapshot VACUUM FULL vectors", error))?;
            rows.push((doc_id, document, vectors));
        }
        rows
    };
    table.clear_documents()?;
    table.clear_text_index()?;
    for index in table.vector_indexes().values_mut() {
        index.clear()?;
    }
    context.storage.clear_btree_indexes(table_name)?;
    table.clear_value_indexes();
    for (doc_id, document, vectors) in documents {
        context
            .rows
            .restore_document(table_name, doc_id, document, vectors)
            .map_err(|error| vacuum_storage_error("rewrite VACUUM FULL row", error))?;
    }
    context
        .rows
        .refresh_indexes(table_name)
        .map_err(|error| vacuum_storage_error("rebuild VACUUM FULL indexes", error))?;
    stats.restore();
    if stats.loaded()
        && !stats.dirty()
        && table.persistence() != uqa_sql::ast::RelationPersistence::Temporary
    {
        stats.persist(table_name)?;
    }
    table.mark_doc_count_dirty();
    context.rows.note_table_data_changed();
    Ok(())
}

pub fn run_vacuum(
    context: &VacuumContext<'_>,
    statement: &VacuumStmt,
) -> Result<SQLResult, SQLError> {
    if context.transactions.depth() != 0 {
        return Err(SQLError::Routine {
            sqlstate: "25001".into(),
            message: "VACUUM cannot run inside a transaction block".into(),
        });
    }

    let execution = uqa_sql::maintenance::analyze_vacuum(statement)?;
    let resolved_targets =
        uqa_sql::maintenance::bind_vacuum_targets(context.catalog, context.privileges, statement)?;

    if execution.only_database_stats() {
        return Ok(SQLResult::empty());
    }

    if execution.full() {
        if resolved_targets.is_empty() {
            context
                .storage
                .vacuum()
                .map_err(|error| SQLError::Internal(format!("VACUUM failed: {error}")))?;
        } else {
            rewrite_full_vacuum_targets(context, &resolved_targets)?;
        }
    }

    if execution.analyze() {
        if resolved_targets.is_empty() {
            for table in context.statistics.table_names("vacuum")? {
                context
                    .statistics
                    .analyze_target(&table, &[], true)
                    .map_err(|error| {
                        SQLError::Internal(format!("VACUUM ANALYZE failed: {error}"))
                    })?;
            }
        } else {
            for target in &resolved_targets {
                context
                    .statistics
                    .analyze_target(&target.table, &target.columns, target.include_descendants)
                    .map_err(|error| {
                        SQLError::Internal(format!("VACUUM ANALYZE failed: {error}"))
                    })?;
            }
        }
    }

    Ok(SQLResult::empty())
}
