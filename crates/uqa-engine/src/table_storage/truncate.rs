//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Clear table-owned storage state through the public Engine table API.

use crate::table_storage::document_store_write_error;
use crate::{DocId, Engine, SQLError};

impl Engine {
    pub fn truncate_table(&self, name: &str) -> Result<(), SQLError> {
        self.truncate_tables(std::slice::from_ref(&name.to_string()))
    }

    pub(crate) fn truncate_tables(&self, names: &[String]) -> Result<(), SQLError> {
        self.truncate_tables_with_identity(names, true)
    }

    pub(crate) fn truncate_tables_with_identity(
        &self,
        names: &[String],
        restart_identity: bool,
    ) -> Result<(), SQLError> {
        if self.transaction_depth() != 0 {
            return self.truncate_tables_inner(names, restart_identity);
        }
        if self.storage.backend.is_none() {
            return self.truncate_tables_inner(names, restart_identity);
        }
        self.begin_implicit_statement_transaction(false)?;
        match self.truncate_tables_inner(names, restart_identity) {
            Ok(()) => self.run_transaction_statement(uqa_sql::ast::TransactionStmt::Commit),
            Err(error) => {
                match self.run_transaction_statement(uqa_sql::ast::TransactionStmt::Rollback) {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(SQLError::Internal(format!(
                        "TRUNCATE failed: {error}; rollback also failed: {rollback_error}"
                    ))),
                }
            }
        }
    }

    fn truncate_tables_inner(
        &self,
        names: &[String],
        restart_identity: bool,
    ) -> Result<(), SQLError> {
        let mut ordered = Vec::new();
        let mut lock_order = std::collections::BTreeSet::new();
        for name in names {
            let Some((table_name, "table")) = self.try_resolve_visible_relation_kind(name)? else {
                return Err(SQLError::UnknownTable(name.to_string()));
            };
            if lock_order.insert(table_name.clone()) {
                ordered.push(table_name);
            }
        }
        for table_name in &lock_order {
            self.lock_relation(
                table_name,
                crate::row_locks::RelationLockMode::AccessExclusive,
            )?;
        }
        if !ordered.is_empty() {
            self.prepare_explicit_transaction_writer()?;
        }
        for table_name in ordered {
            self.truncate_locked_table(&table_name, restart_identity)?;
        }
        Ok(())
    }

    pub(crate) fn truncate_locked_table(
        &self,
        table_name: &str,
        restart_identity: bool,
    ) -> Result<(), SQLError> {
        let t = self.require_table(table_name)?;
        *t.storage_generation.write() = crate::new_table_storage_generation().map_err(|error| {
            SQLError::Internal(format!("rotate TRUNCATE storage generation: {error}"))
        })?;
        self.try_save_table_schema(table_name, &t)
            .map_err(|error| {
                SQLError::Internal(format!("persist TRUNCATE storage generation: {error}"))
            })?;
        // Snapshot the doc id set before grabbing any write locks so
        // we do not deadlock against the read guard inside the loop.
        let ids: Vec<DocId> = t
            .document_store
            .read()
            .doc_ids()
            .map_err(|error| SQLError::Internal(format!("read document ids: {error}")))?;
        let removed_count = (ids.len() as u64).max(1);
        for doc_id in ids {
            t.document_store
                .write()
                .delete(doc_id)
                .map_err(|err| document_store_write_error(&err))?;
            t.inverted_index
                .write()
                .remove_document(doc_id)
                .map_err(|error| SQLError::Internal(format!("remove indexed document: {error}")))?;
            for idx in t.vector_indexes.write().values_mut() {
                idx.as_mut().delete(doc_id).map_err(|error| {
                    SQLError::Internal(format!("delete indexed vector: {error}"))
                })?;
            }
            self.note_row_deleted(table_name, doc_id)?;
        }
        if restart_identity {
            *t.next_id.lock() = 1;
            self.persist_next_id(table_name).map_err(|error| {
                SQLError::Internal(format!("persist TRUNCATE identity: {error}"))
            })?;
            let owned_sequences = self
                .sequence_names_owned_by_tables(&std::collections::BTreeSet::from([t.object_id()]))
                .map_err(|error| SQLError::Internal(format!("load owned sequences: {error}")))?;
            for sequence in owned_sequences {
                self.restart_owned_sequence(&sequence).map_err(|error| {
                    SQLError::Internal(format!("restart owned sequence `{sequence}`: {error}"))
                })?;
            }
        }
        self.value_indexes_truncate(table_name, &t)?;
        self.mark_column_stats_dirty_by_count(table_name, &t, removed_count)
            .map_err(|err| SQLError::Internal(format!("invalidate column stats: {err}")))?;
        Ok(())
    }
}
