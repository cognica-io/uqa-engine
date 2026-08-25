//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation and column lifecycle handling for stored triggers.

use std::collections::BTreeMap;

use uqa_sql::ast::DropTrigger;
use uqa_sql::SQLError;

use crate::{Engine, RelationIdentity, StorageBackendError, StorageBackendResult};

impl Engine {
    pub(crate) fn drop_relation_triggers_inner(
        &self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<()> {
        let mut triggers = self.durable.triggers.write();
        if !triggers.contains_key(relation) {
            return Ok(());
        }
        let mut next = triggers.clone();
        next.remove(relation);
        self.persist_trigger_catalog_snapshot(&next)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        *triggers = next;
        drop(triggers);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn rename_relation_triggers_inner(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> StorageBackendResult<()> {
        let mut triggers = self.durable.triggers.write();
        if !triggers.contains_key(from) {
            return Ok(());
        }
        let mut next = triggers.clone();
        if let Some(mut entries) = next.remove(from) {
            for trigger in entries.values_mut() {
                trigger.definition.table = to.qualified_name();
            }
            next.insert(to.clone(), entries);
        }
        self.persist_trigger_catalog_snapshot(&next)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        *triggers = next;
        drop(triggers);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn rename_trigger_column_inner(
        &self,
        table: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        let relation =
            RelationIdentity::from_legacy_name(table).map_err(StorageBackendError::Other)?;
        let mut triggers = self.durable.triggers.write();
        if !triggers.contains_key(&relation) {
            return Ok(());
        }
        let mut next = triggers.clone();
        if let Some(entries) = next.get_mut(&relation) {
            for trigger in entries.values_mut() {
                for column in &mut trigger.definition.update_columns {
                    if column == from {
                        *column = to.to_string();
                    }
                }
                if let Some(condition) = trigger.definition.when.as_mut() {
                    crate::engine_table_storage::rename_schema_expr_column(condition, from, to)?;
                }
            }
        }
        self.persist_trigger_catalog_snapshot(&next)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        *triggers = next;
        drop(triggers);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn handle_drop_column_trigger_dependencies(
        &self,
        table: &str,
        column: &str,
        cascade: bool,
    ) -> Result<(), SQLError> {
        let relation = RelationIdentity::from_legacy_name(table).map_err(|error| {
            SQLError::Internal(format!("decode trigger relation `{table}`: {error}"))
        })?;
        let dependent = self
            .durable
            .triggers
            .read()
            .get(&relation)
            .into_iter()
            .flat_map(BTreeMap::values)
            .filter(|trigger| {
                trigger
                    .definition
                    .update_columns
                    .iter()
                    .any(|name| name == column)
                    || trigger.definition.when.as_ref().is_some_and(|condition| {
                        crate::engine_table_storage::schema_expr_references_column(
                            condition, column,
                        )
                    })
            })
            .map(|trigger| trigger.definition.name.clone())
            .collect::<Vec<_>>();
        if dependent.is_empty() {
            return Ok(());
        }
        if !cascade {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "cannot drop column {column} of table {table} because trigger {} depends on it",
                    dependent.join(", ")
                ),
            });
        }
        for name in dependent {
            self.drop_trigger(&DropTrigger {
                name: name.clone(),
                table: table.to_string(),
                if_exists: false,
                cascade: true,
            })?;
            self.push_sql_notice(
                "NOTICE",
                &format!("drop cascades to trigger {name} on table {table}"),
            );
        }
        Ok(())
    }
}
