//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column definitions that embed the catalog names of user-defined types.

use crate::{Engine, StorageBackendResult};
use uqa_core::RelationIdentity;

fn rename_column_types(
    columns: &mut [uqa_sql::ast::ColumnDef],
    oid: u32,
    identity: &RelationIdentity,
) -> bool {
    let mut changed = false;
    for column in columns {
        changed |= column.ty.rename_user_type(oid, identity);
    }
    changed
}

impl Engine {
    /// Give table and foreign table columns of the user-defined type `oid` the type's current catalog name, persisting changed definitions before publishing them.
    pub(crate) fn rewrite_schema_type_references(
        &self,
        oid: u32,
        identity: &RelationIdentity,
    ) -> StorageBackendResult<()> {
        let mut table_updates = Vec::new();
        for (table_name, table) in self.table_entries() {
            let mut columns = table.columns.read().clone();
            if rename_column_types(&mut columns, oid, identity) {
                table_updates.push((table_name, table, columns));
            }
        }
        let mut foreign_updates = Vec::new();
        let foreign = self.durable.foreign_catalog.read().clone();
        for (relation, table) in foreign.tables() {
            let mut table = table.clone();
            if rename_column_types(&mut table.columns, oid, identity) {
                foreign_updates.push((relation.clone(), table));
            }
        }
        let next_foreign = foreign
            .with_table_updates(&foreign_updates)
            .map_err(|error| {
                uqa_storage::StorageBackendError::backend("admit foreign dependency changes", error)
            })?;
        if self.is_persistent() {
            for (table_name, table, columns) in &table_updates {
                self.persist_constraint_candidate(
                    table_name,
                    table,
                    columns,
                    &table.table_checks.read(),
                    &table.foreign_keys.read(),
                    &table.key_constraints.read(),
                )?;
            }
            for (relation, table) in &foreign_updates {
                self.foreign_definition_context()
                    .persist_foreign_table_definition(relation, table)?;
            }
        }
        let tables_changed = !table_updates.is_empty();
        for (_, table, columns) in table_updates {
            table.columns.write().publish(columns)?;
        }
        let foreign_tables_changed = !foreign_updates.is_empty();
        if foreign_tables_changed {
            *self.durable.foreign_catalog.write() = next_foreign;
        }
        if tables_changed {
            self.note_table_catalog_changed();
        }
        if foreign_tables_changed {
            self.note_catalog_registry_changed();
        }
        Ok(())
    }
}
