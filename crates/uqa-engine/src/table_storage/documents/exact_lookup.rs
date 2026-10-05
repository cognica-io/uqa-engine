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
    mutation::overlay::CommandMutationOverlay,
    query::document_changes::DocumentChanges,
    query::exact_lookup::{ExactLookup, ExactLookupOverlay, FieldPresence},
    serializable::SerializableRelationRead,
    storage_errors::storage_error,
};
use uqa_sql::SQLError;
use uqa_storage::ValueIndexKey;

struct CommandOverlay<'a> {
    engine: &'a Engine,
    table: String,
}

impl ExactLookupOverlay for CommandOverlay<'_> {
    fn is_empty(&self) -> Result<bool, SQLError> {
        Ok(!self
            .engine
            .session
            .command_mutation_overlays
            .lock()
            .iter()
            .any(|overlay| overlay.holds(&self.table)))
    }

    fn masks(&self, doc_id: DocId) -> Result<bool, SQLError> {
        let control = self.engine.query_retention_control()?;
        CommandMutationOverlay::stages(
            &self.engine.session.command_mutation_overlays.lock(),
            &self.table,
            doc_id,
            &control,
        )
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

/// The changes a read merges for a table: this transaction's rows that a fixed snapshot does not show, below the rows the running commands staged, whose exact indexes answer key probes.
struct QueryOverlay<'a> {
    fixed: DocumentChanges,
    commands: CommandOverlay<'a>,
}

impl<'a> QueryOverlay<'a> {
    fn new(engine: &'a Engine, table: &str) -> Result<Self, SQLError> {
        let canonical = engine.command_overlay_table_name(table)?;
        Ok(Self {
            fixed: engine
                .fixed_transaction_row_changes(&canonical)?
                .unwrap_or_default(),
            commands: CommandOverlay {
                engine,
                table: canonical,
            },
        })
    }
}

impl ExactLookupOverlay for QueryOverlay<'_> {
    fn is_empty(&self) -> Result<bool, SQLError> {
        Ok(!self.fixed.has_changes() && self.commands.is_empty()?)
    }

    fn masks(&self, doc_id: DocId) -> Result<bool, SQLError> {
        Ok(self.commands.masks(doc_id)? || self.fixed.masks(doc_id)?)
    }

    /// The smallest visible identity whose row matches: a staged row, or a fixed-snapshot row that no command restaged.
    fn find_match(
        &self,
        columns: &[String],
        values: &[Value],
        presence: FieldPresence,
    ) -> Result<Option<DocId>, SQLError> {
        let staged = self.commands.find_match(columns, values, presence)?;
        if !self.fixed.has_changes() {
            return Ok(staged);
        }
        for change in self.fixed.changes() {
            let (id, present) =
                change.map_err(|error| storage_error("read private exact key", &error))?;
            if staged.is_some_and(|staged| id >= staged) {
                break;
            }
            if !present || self.commands.masks(id)? {
                continue;
            }
            if self.fixed.row_matches(id, columns, values, presence)? {
                return Ok(Some(id));
            }
        }
        Ok(staged)
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
            let overlay = QueryOverlay::new(engine, name)?;
            let read = engine.serializable_table_state_read(table)?;
            ExactLookup {
                table: table.as_ref(),
                overlay: &overlay,
                read: read.as_ref(),
            }
            .find_field_with_index(field, value, |field, value| {
                engine.read_value_index_state(
                    name,
                    table,
                    &ValueIndexKey::Column(field.into()),
                    |index| Ok(index.field_candidates(value)),
                )
            })
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
        .find_field_with_index(field, value, |field, value| {
            self.read_value_index_state(
                table,
                &table_state,
                &ValueIndexKey::Column(field.into()),
                |index| Ok(index.field_candidates(value)),
            )
        })
    }

    /// Find a matching conflict key in the caller's transaction snapshot. Execution selects the integer primary-key mapping, an answerable value index, or an evaluated scan and records the corresponding serializable read.
    pub fn find_conflict(
        &self,
        table: &str,
        columns: &[String],
        values: &[Value],
    ) -> Result<Option<DocId>, SQLError> {
        self.with_direct_table_read(table, |engine, name, table| {
            let overlay = QueryOverlay::new(engine, name)?;
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
