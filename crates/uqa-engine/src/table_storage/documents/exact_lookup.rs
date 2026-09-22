//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply the selected table, private rows and transaction reader to execution-owned exact lookups.

use crate::{Engine, TableState};
use std::sync::Arc;
use uqa_core::{DocId, Value};
use uqa_execution::{
    query::exact_lookup::{ExactLookup, ExactLookupOverlay, FieldPresence},
    serializable::SerializableRelationRead,
};
use uqa_sql::SQLError;
use uqa_storage::ValueIndexKey;

struct CommandOverlay<'a> {
    engine: &'a Engine,
    table: String,
}

impl ExactLookupOverlay for CommandOverlay<'_> {
    fn is_empty(&self) -> Result<bool, SQLError> {
        Ok(self
            .engine
            .session
            .command_mutation_overlays
            .lock()
            .iter()
            .all(|overlay| {
                overlay
                    .documents(&self.table)
                    .is_none_or(std::collections::BTreeMap::is_empty)
            }))
    }

    fn masks(&self, doc_id: DocId) -> Result<bool, SQLError> {
        Ok(self
            .engine
            .session
            .command_mutation_overlays
            .lock()
            .iter()
            .any(|overlay| {
                overlay
                    .documents(&self.table)
                    .is_some_and(|documents| documents.contains_key(&doc_id))
            }))
    }

    fn find_match(
        &self,
        columns: &[String],
        values: &[Value],
        presence: FieldPresence,
    ) -> Result<Option<DocId>, SQLError> {
        self.engine
            .command_overlay_exact_match(&self.table, columns, values, presence)
    }
}

impl Engine {
    pub fn find_doc_id_by_field(
        &self,
        table: &str,
        field: &str,
        value: &Value,
    ) -> Result<Option<DocId>, SQLError> {
        self.with_direct_table_read(table, |engine, name, table| {
            let overlay = engine.command_overlay_changes(name)?.unwrap_or_default();
            let read = engine.serializable_table_state_read(table)?;
            ExactLookup {
                table: table.as_ref(),
                overlay: &overlay,
                read: read.as_ref(),
            }
            .find_field(field, value)
        })
    }

    pub(crate) fn find_mutation_doc_id_by_field(
        &self,
        table: &str,
        field: &str,
        value: &Value,
    ) -> Result<Option<DocId>, SQLError> {
        let table_state = self.require_table(table)?;
        let overlay = CommandOverlay {
            engine: self,
            table: self.command_overlay_table_name(table)?,
        };
        ExactLookup {
            table: table_state.as_ref(),
            overlay: &overlay,
            read: None,
        }
        .find_field(field, value)
    }

    /// Find a matching conflict key in the caller's transaction snapshot. Execution selects the integer primary-key mapping, an answerable value index, or an evaluated scan and records the corresponding serializable read.
    pub fn find_conflict(
        &self,
        table: &str,
        columns: &[String],
        values: &[Value],
    ) -> Result<Option<DocId>, SQLError> {
        self.with_direct_table_read(table, |engine, name, table| {
            let overlay = engine.command_overlay_changes(name)?.unwrap_or_default();
            let read = engine.serializable_table_state_read(table)?;
            engine.find_conflict_in_state(name, table, columns, values, &overlay, read.as_ref())
        })
    }

    pub(crate) fn find_mutation_conflict(
        &self,
        table: &str,
        columns: &[String],
        values: &[Value],
    ) -> Result<Option<DocId>, SQLError> {
        let table_state = self.require_table(table)?;
        let overlay = CommandOverlay {
            engine: self,
            table: self.command_overlay_table_name(table)?,
        };
        self.find_conflict_in_state(table, &table_state, columns, values, &overlay, None)
    }

    fn find_conflict_in_state(
        &self,
        name: &str,
        table: &Arc<TableState>,
        columns: &[String],
        values: &[Value],
        overlay: &dyn ExactLookupOverlay,
        read: Option<&SerializableRelationRead>,
    ) -> Result<Option<DocId>, SQLError> {
        let schema_columns = table.columns.snapshot();
        ExactLookup {
            table: table.as_ref(),
            overlay,
            read,
        }
        .find_conflict(&schema_columns, columns, values, |column, predicate| {
            self.value_index_scan_state(
                name,
                table,
                &ValueIndexKey::Column(column.into()),
                predicate,
                read,
            )
        })
    }
}
