//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind public document reads to their transaction and retain unscoped query and mutation views.

use crate::{Engine, TableState};
use std::sync::Arc;
use uqa_sql::SQLError;
use uqa_storage::{document_store::Document, InvertedIndex};

impl Engine {
    fn with_direct_table_read<R>(
        &self,
        name: &str,
        read: impl FnOnce(&Self, &str, &Arc<TableState>) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        self.with_direct_read_snapshot(|engine| {
            let resolve = || {
                let Some(name) = engine.try_resolve_query_table_name(name).map_err(|error| {
                    uqa_execution::storage_errors::storage_error(
                        "resolve direct read table",
                        &error,
                    )
                })?
                else {
                    return Ok(None);
                };
                let table = engine.require_query_table(&name)?;
                Ok(Some(uqa_execution::row_locks::binding::RelationBinding {
                    name,
                    object_id: Some(table.object_id()),
                    value: table,
                }))
            };
            // Attached physical readers already retain their source view and have no logical frame whose locks this call could own.
            let binding = if engine.transaction_depth() == 0 {
                resolve()?
            } else {
                uqa_execution::query::table_read::bind_direct_table_read(engine, resolve)?
            }
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
            read(engine, &binding.name, &binding.value)
        })
    }

    pub fn get_document(
        &self,
        table: &str,
        doc_id: uqa_core::DocId,
    ) -> Result<Option<Document>, SQLError> {
        self.with_direct_table_read(table, |engine, name, table| {
            if let Some(read) = engine.serializable_table_state_read(table)? {
                read.observe_row(doc_id)?;
            }
            engine.materialized_document_from_state(name, table, doc_id)
        })
    }

    pub(crate) fn get_live_document(
        &self,
        table: &str,
        doc_id: uqa_core::DocId,
    ) -> Result<Option<Document>, SQLError> {
        let state = self.require_table(table)?;
        self.materialized_document_from_state(table, &state, doc_id)
    }

    pub(crate) fn get_query_document(
        &self,
        table: &str,
        doc_id: uqa_core::DocId,
    ) -> Result<Option<Document>, SQLError> {
        let state = self.require_query_table(table)?;
        self.materialized_document_from_state(table, &state, doc_id)
    }

    fn materialized_document_from_state(
        &self,
        name: &str,
        table: &TableState,
        doc_id: uqa_core::DocId,
    ) -> Result<Option<Document>, SQLError> {
        let mut document = self.raw_command_visible_document(name, table, doc_id)?;
        if let Some(document) = document.as_mut() {
            Self::materialize_query_document(&table.columns.read(), document)?;
        }
        Ok(document.map(uqa_storage::StoredDocument::into_fields))
    }

    /// Count documents retained by the table's text index in the selected transaction view.
    pub fn document_count(&self, table: &str) -> Result<u64, SQLError> {
        self.with_direct_table_read(table, |engine, _, table| {
            let index = table.inverted_index.read();
            let index = uqa_execution::serializable::text::ObservedTextIndex::new(
                index.as_ref(),
                engine.serializable_table_state_read(table)?,
                table.columns.snapshot(),
            );
            index.doc_count().map_err(|error| {
                uqa_execution::storage_errors::storage_error("read indexed document count", &error)
            })
        })
    }

    /// All document ids in the caller's selected transaction view.
    pub fn table_doc_ids(&self, table: &str) -> Result<Vec<uqa_core::DocId>, SQLError> {
        self.with_direct_table_read(table, |engine, name, table| {
            if let Some(read) = engine.serializable_table_state_read(table)? {
                read.observe_scan()?;
            }
            engine.table_doc_ids_from_state(name, table)
        })
    }

    pub(crate) fn query_table_doc_ids(
        &self,
        table: &str,
    ) -> Result<Vec<uqa_core::DocId>, SQLError> {
        let Some(t) = self
            .try_query_table(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        self.table_doc_ids_from_state(table, &t)
    }

    /// All current document ids for schema mutation and validation. Unlike the SELECT-facing path, DDL must inspect rows committed after a fixed transaction snapshot was acquired.
    pub(crate) fn live_table_doc_ids(&self, table: &str) -> Result<Vec<uqa_core::DocId>, SQLError> {
        let Some(t) = self.try_table(table).map_err(|error| {
            SQLError::Internal(format!("resolve live table `{table}`: {error}"))
        })?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        self.table_doc_ids_from_state(table, &t)
    }

    fn table_doc_ids_from_state(
        &self,
        table: &str,
        state: &crate::TableState,
    ) -> Result<Vec<uqa_core::DocId>, SQLError> {
        let doc_ids = state
            .document_store
            .read()
            .doc_ids()
            .map_err(|error| SQLError::Internal(format!("read document ids: {error}")))?;
        let Some(changes) = self.command_overlay_changes(table)? else {
            return Ok(doc_ids);
        };
        let mut visible = doc_ids
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        for (doc_id, document) in changes {
            if document.is_some() {
                visible.insert(doc_id);
            } else {
                visible.remove(&doc_id);
            }
        }
        Ok(visible.into_iter().collect())
    }

    pub(crate) fn table_doc_count(&self, table: &str) -> Result<u64, SQLError> {
        use std::sync::atomic::Ordering;
        let Some(t) = self
            .try_query_table(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        if let Some(changes) = self
            .command_overlay_changes(table)?
            .filter(|changes| !changes.is_empty())
        {
            let store = t.document_store.read();
            let mut count =
                u64::try_from(store.len().map_err(|error| {
                    SQLError::Internal(format!("read document count: {error}"))
                })?)
                .map_err(|_| SQLError::Internal("document count exceeds u64".into()))?;
            for (doc_id, document) in changes {
                let persisted = store.contains_doc_id(doc_id).map_err(|error| {
                    SQLError::Internal(format!("read command-visible document count: {error}"))
                })?;
                match (persisted, document.is_some()) {
                    (false, true) => {
                        count = count.checked_add(1).ok_or_else(|| {
                            SQLError::Internal("document count exceeds u64".into())
                        })?;
                    }
                    (true, false) => {
                        count = count
                            .checked_sub(1)
                            .ok_or_else(|| SQLError::Internal("document count underflow".into()))?;
                    }
                    _ => {}
                }
            }
            return Ok(count);
        }
        if !t.doc_count_dirty.load(Ordering::Acquire) {
            return Ok(t.doc_count_cache.load(Ordering::Acquire));
        }
        let count = t
            .document_store
            .read()
            .len()
            .map_err(|error| SQLError::Internal(format!("read document count: {error}")))?;
        let count = u64::try_from(count)
            .map_err(|_| SQLError::Internal("document count exceeds u64".into()))?;
        t.doc_count_cache.store(count, Ordering::Release);
        t.doc_count_dirty.store(false, Ordering::Release);
        Ok(count)
    }
}
