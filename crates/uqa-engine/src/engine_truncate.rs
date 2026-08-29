//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{DocId, Engine, SQLError};
use crate::engine_table_storage::document_store_write_error;

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
            let table_name = self
                .try_resolve_table_name(name)
                .map_err(|error| SQLError::Internal(format!("resolve table `{name}`: {error}")))?
                .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
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
            let owned_sequences = t
                .columns
                .read()
                .iter()
                .filter_map(|column| {
                    let provenance = column.auto_increment.as_ref()?;
                    let owner = provenance.owner.as_ref()?;
                    if owner.table == table_name && owner.column == column.name {
                        provenance.sequence.clone()
                    } else {
                        None
                    }
                })
                .collect::<std::collections::BTreeSet<_>>();
            for sequence in owned_sequences {
                self.restart_owned_sequence(&sequence).map_err(|error| {
                    SQLError::Internal(format!("restart owned sequence `{sequence}`: {error}"))
                })?;
            }
        }
        self.value_indexes_truncate(table_name, &t)?;
        self.mark_column_stats_dirty(table_name, &t)
            .map_err(|err| SQLError::Internal(format!("invalidate column stats: {err}")))?;
        Ok(())
    }
}
