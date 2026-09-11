//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable table-column dependencies for SQL sequences.

use super::{Engine, SequenceOwner, StorageBackendError, StorageBackendResult};

impl Engine {
    pub(crate) fn sequence_names_owned_by_tables(
        &self,
        table_object_ids: &std::collections::BTreeSet<[u8; 16]>,
    ) -> StorageBackendResult<std::collections::BTreeSet<String>> {
        uqa_execution::catalog::sequence_introspection::ownership::sequence_names_owned_by_tables(
            self,
            table_object_ids,
        )
    }

    pub(crate) fn sequence_external_dependents_for_owner_drop(
        &self,
        sequence: &str,
        owner_drop_targets: &std::collections::BTreeSet<String>,
    ) -> StorageBackendResult<Vec<String>> {
        let mut dependents = self
            .sequence_schema_expression_dependents(sequence)?
            .into_iter()
            .filter(|dependency| !owner_drop_targets.contains(dependency.table()))
            .map(|dependency| dependency.object_label())
            .collect::<Vec<_>>();
        dependents.extend(
            self.views_depending_on_sequence(sequence)?
                .into_iter()
                .map(|view| format!("view {view}")),
        );
        dependents.extend(
            self.event_lookup_context()
                .rules_depending_on_relations(&[sequence.to_string()])
                .map_err(uqa_storage::StorageBackendError::Other)?
                .into_iter()
                .map(|(table, rule)| format!("rule {rule} on table {}", table.qualified_name())),
        );
        dependents.sort_unstable();
        dependents.dedup();
        Ok(dependents)
    }

    pub(crate) fn sequence_names_owned_by_column(
        &self,
        table_object_id: [u8; 16],
        column_object_id: [u8; 16],
    ) -> StorageBackendResult<std::collections::BTreeSet<String>> {
        uqa_execution::catalog::sequence_introspection::ownership::sequence_names_owned_by_column(
            self,
            table_object_id,
            column_object_id,
        )
    }

    pub(crate) fn owned_sequence_dependents_for_column(
        &self,
        table_name: &str,
        column_name: &str,
    ) -> StorageBackendResult<Vec<String>> {
        let canonical = self.try_resolve_table_name(table_name)?.ok_or_else(|| {
            StorageBackendError::Other(format!("table `{table_name}` does not exist"))
        })?;
        let table = self.try_table(&canonical)?.ok_or_else(|| {
            StorageBackendError::Other(format!("table `{canonical}` disappeared"))
        })?;
        let column_object_id = table
            .columns
            .read()
            .iter()
            .find(|column| column.name == column_name)
            .and_then(|column| column.object_id)
            .ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "column `{canonical}`.`{column_name}` has no object identity"
                ))
            })?;
        let mut dependents = Vec::new();
        for sequence in self.sequence_names_owned_by_column(table.object_id(), column_object_id)? {
            dependents.extend(
                self.sequence_schema_expression_dependents(&sequence)?
                    .into_iter()
                    .filter(|dependency| !dependency.is_column(&canonical, column_name))
                    .map(|dependency| dependency.object_label()),
            );
            dependents.extend(
                self.views_depending_on_sequence(&sequence)?
                    .into_iter()
                    .map(|view| format!("view {view}")),
            );
        }
        dependents.sort_unstable();
        dependents.dedup();
        Ok(dependents)
    }

    pub(crate) fn sequence_owner_target(
        &self,
        owner: SequenceOwner,
    ) -> Option<(String, String, bool)> {
        self.table_entries()
            .into_iter()
            .find_map(|(table_name, table)| {
                if table.object_id() != owner.table_object_id {
                    return None;
                }
                table
                    .columns
                    .read()
                    .iter()
                    .find(|column| column.object_id == Some(owner.column_object_id))
                    .map(|column| (table_name, column.name.clone(), false))
            })
            .or_else(|| {
                self.durable
                    .foreign_tables
                    .read()
                    .iter()
                    .find_map(|(relation, table)| {
                        if table.object_id != owner.table_object_id {
                            return None;
                        }
                        table
                            .columns
                            .iter()
                            .find(|column| column.object_id == Some(owner.column_object_id))
                            .map(|column| (relation.qualified_name(), column.name.clone(), true))
                    })
            })
    }
}
