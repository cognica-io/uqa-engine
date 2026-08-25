//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table drop, lookup, description, and default inspection.

use super::{
    stored_relation_reference_matches, table_not_found, Arc, Engine, RelationIdentity,
    StorageBackendError, StorageBackendResult, TableState,
};

impl Engine {
    /// Drop a table from the catalog and release its in-memory state.
    /// Returns `true` if the table existed.
    pub fn drop_table(&self, name: &str) -> StorageBackendResult<bool> {
        self.try_drop_table(name)
    }

    pub(crate) fn try_drop_table(&self, name: &str) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            let Some(name) = engine.resolve_table_ddl_target(name, "DROP TABLE")? else {
                return Ok(false);
            };
            engine.try_drop_tables_inner(&[name], false)?;
            Ok(true)
        })
    }

    pub(crate) fn try_drop_tables(
        &self,
        names: &[String],
        cascade: bool,
    ) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(|engine| {
            engine.try_drop_tables_inner(names, cascade)
        })
    }

    pub(super) fn canonical_drop_table_names(
        &self,
        names: &[String],
    ) -> StorageBackendResult<Vec<String>> {
        let mut canonical_names = Vec::with_capacity(names.len());
        for name in names {
            canonical_names.push(
                self.resolve_table_ddl_target(name, "DROP TABLE")?
                    .ok_or_else(|| table_not_found(name))?,
            );
        }
        canonical_names.sort_unstable();
        canonical_names.dedup();
        Ok(canonical_names)
    }

    fn drop_target_sets(
        canonical_names: &[String],
    ) -> StorageBackendResult<(std::collections::BTreeSet<String>, Vec<RelationIdentity>)> {
        let target_names = canonical_names.iter().cloned().collect();
        let targets = canonical_names
            .iter()
            .map(|name| Self::resolved_relation_identity(name))
            .collect::<StorageBackendResult<Vec<_>>>()?;
        Ok((target_names, targets))
    }

    fn ensure_no_drop_view_dependencies(
        &self,
        canonical_names: &[String],
    ) -> StorageBackendResult<()> {
        for name in canonical_names {
            self.ensure_no_dependent_views("DROP TABLE", name)?;
        }
        Ok(())
    }

    pub(super) fn try_drop_tables_inner(
        &self,
        names: &[String],
        cascade: bool,
    ) -> StorageBackendResult<()> {
        let canonical_names = self.canonical_hierarchy_drop_targets(names, cascade)?;
        let (target_names, targets) = Self::drop_target_sets(&canonical_names)?;

        // Finish every dependency check before mutating a referrer or target.
        self.ensure_no_drop_view_dependencies(&canonical_names)?;
        let entries = self.table_entries();
        Self::ensure_drop_targets_unreferenced(&target_names, &targets, &entries)?;
        let owned_sequences = entries
            .iter()
            .filter(|(table, _)| target_names.contains(table))
            .flat_map(|(table, state)| {
                state
                    .columns
                    .read()
                    .iter()
                    .filter_map(|column| {
                        let provenance = column.auto_increment.as_ref()?;
                        let owner = provenance.owner.as_ref()?;
                        if owner.table == *table && owner.column == column.name {
                            provenance.sequence.clone()
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<std::collections::BTreeSet<_>>();

        let mut inbound = Vec::new();
        let mut updates = Vec::new();
        for (candidate_name, table) in entries {
            if target_names.contains(&candidate_name) {
                continue;
            }
            let mut columns = table.columns.read().clone();
            let checks = table.table_checks.read().clone();
            let mut foreign_keys = table.foreign_keys.read().clone();
            let key_constraints = table.key_constraints.read().clone();
            let previous_fk_len = foreign_keys.len();
            foreign_keys.retain(|foreign_key| {
                !targets
                    .iter()
                    .any(|target| Self::foreign_key_targets(foreign_key, target))
            });
            let mut changed = previous_fk_len != foreign_keys.len();
            for column in &mut columns {
                if column.references.as_ref().is_some_and(|reference| {
                    targets
                        .iter()
                        .any(|target| stored_relation_reference_matches(&reference.table, target))
                }) {
                    column.references = None;
                    changed = true;
                }
            }
            if changed {
                inbound.push(candidate_name.clone());
                updates.push((
                    candidate_name,
                    table,
                    columns,
                    checks,
                    foreign_keys,
                    key_constraints,
                ));
            }
        }
        if !cascade && !inbound.is_empty() {
            inbound.sort_unstable();
            inbound.dedup();
            return Err(StorageBackendError::Other(format!(
                "DROP TABLE rejected: still referenced by foreign key(s) on `{}`; use CASCADE",
                inbound.join("`, `")
            )));
        }
        if cascade {
            for (name, table, columns, checks, foreign_keys, key_constraints) in &updates {
                self.persist_constraint_candidate(
                    name,
                    table,
                    columns,
                    checks,
                    foreign_keys,
                    key_constraints,
                )?;
            }
            for (_, table, columns, checks, foreign_keys, _) in updates {
                *table.columns.write() = columns;
                *table.table_checks.write() = checks;
                *table.foreign_keys.write() = foreign_keys;
            }
        }
        for name in canonical_names {
            self.drop_table_state_inner(&name)?;
        }
        for sequence in owned_sequences {
            self.drop_owned_sequence(&sequence)?;
        }
        Ok(())
    }

    fn ensure_drop_targets_unreferenced(
        target_names: &std::collections::BTreeSet<String>,
        targets: &[RelationIdentity],
        entries: &[(String, Arc<TableState>)],
    ) -> StorageBackendResult<()> {
        for (candidate_name, table) in entries {
            if target_names.contains(candidate_name) {
                continue;
            }
            if let Some(target) = targets
                .iter()
                .find(|target| Self::table_schema_references_relation(table, target))
            {
                return Err(StorageBackendError::Other(format!(
                    "DROP TABLE `{}` rejected: schema expression on `{candidate_name}` may depend on it and cannot be rewritten safely",
                    target.qualified_name()
                )));
            }
        }
        Ok(())
    }

    fn canonical_hierarchy_drop_targets(
        &self,
        names: &[String],
        cascade: bool,
    ) -> StorageBackendResult<Vec<String>> {
        let canonical_names = self.canonical_drop_table_names(names)?;
        let (canonical_names, hierarchy_dependents) =
            self.hierarchy_drop_targets(&canonical_names, cascade);
        if !hierarchy_dependents.is_empty() {
            return Err(StorageBackendError::Other(format!(
                "DROP TABLE rejected: table `{}` depends on the target through inheritance; use CASCADE",
                hierarchy_dependents.join("`, `")
            )));
        }
        Ok(canonical_names)
    }

    pub(crate) fn drop_table_state_inner(&self, name: &str) -> StorageBackendResult<()> {
        let relation = Self::resolved_relation_identity(name)?;
        if !self.storage.tables.read().contains_key(&relation) {
            return Err(table_not_found(name));
        }
        let temporary = self
            .storage
            .tables
            .read()
            .get(&relation)
            .is_some_and(|table| table.persistence == uqa_sql::ast::RelationPersistence::Temporary);
        if !temporary {
            if let Some(catalog) = self.storage.catalog.as_ref() {
                catalog.drop_table_and_data(name)?;
                self.note_table_catalog_changed();
            }
        }
        self.storage.tables.write().remove(&relation);
        if temporary {
            self.note_table_catalog_changed();
        }
        // Sweep every related per-table registry so catalog state
        // does not outlive the table.
        self.durable
            .table_field_analyzers
            .write()
            .retain(|(t, _), _| t != name);
        self.durable
            .catalog_indexes
            .write()
            .retain(|_, row| row.table_name != name);
        Ok(())
    }

    pub(crate) fn drop_temporary_table_on_commit_inner(
        &self,
        name: &str,
    ) -> StorageBackendResult<()> {
        self.drop_temporary_views_depending_on_relation_inner(name)?;
        self.try_drop_tables_inner(&[name.to_string()], true)
    }

    pub fn has_table(&self, name: &str) -> StorageBackendResult<bool> {
        self.try_has_table(name)
    }

    pub fn try_has_table(&self, name: &str) -> StorageBackendResult<bool> {
        Ok(self.try_resolve_table_name(name)?.is_some())
    }

    /// All schema-declared columns for `table`, in declaration order.
    pub fn table_columns(&self, table: &str) -> StorageBackendResult<Vec<String>> {
        self.try_table_columns(table)
    }

    pub fn try_table_columns(&self, table: &str) -> StorageBackendResult<Vec<String>> {
        let table_state = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let columns = table_state
            .columns
            .read()
            .iter()
            .map(|column| column.name.clone())
            .collect();
        Ok(columns)
    }

    pub fn table_has_column(&self, table: &str, column: &str) -> StorageBackendResult<bool> {
        self.try_table_has_column(table, column)
    }

    pub fn try_table_has_column(&self, table: &str, column: &str) -> StorageBackendResult<bool> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let cols = t.columns.read();
        Ok(cols.iter().any(|c| c.name == column))
    }

    pub(crate) fn column_type(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Option<uqa_sql::ast::ColumnType>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let cols = t.columns.read();
        Ok(cols.iter().find(|c| c.name == column).map(|c| c.ty.clone()))
    }

    /// Return the first SERIAL or identity column name for `table`, if any.
    pub(crate) fn auto_increment_column(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<String>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let cols = t.columns.read();
        Ok(cols
            .iter()
            .find(|c| c.auto_increment.is_some())
            .map(|c| c.name.clone()))
    }

    /// Sequence-generating columns and their durable provenance, in schema order. More than one `SERIAL`/identity column may exist on a table even though only the first one is used as the engine's physical document id.
    pub(crate) fn auto_increment_columns(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<(String, uqa_sql::ast::AutoIncrement)>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let columns = t
            .columns
            .read()
            .iter()
            .filter_map(|column| {
                column
                    .auto_increment
                    .clone()
                    .map(|provenance| (column.name.clone(), provenance))
            })
            .collect();
        Ok(columns)
    }

    /// Sorted list of every registered table name.
    pub fn table_names(&self) -> StorageBackendResult<Vec<String>> {
        self.synchronize_table_catalog()?;
        Ok(self
            .storage
            .tables
            .read()
            .keys()
            .map(RelationIdentity::qualified_name)
            .collect())
    }

    /// Snapshot the column schema of `table`. Returns `None` when no
    /// table by that name is registered.
    pub fn describe_table(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<Vec<uqa_sql::ast::ColumnDef>>> {
        self.try_describe_table(table)
    }

    pub fn try_describe_table(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<Vec<uqa_sql::ast::ColumnDef>>> {
        let Some(table) = self.try_table(table)? else {
            return Ok(None);
        };
        let mut columns = table.columns.read().clone();
        for column in &mut columns {
            if let Some(default) = &mut column.default {
                self.resolve_stored_sequence_references_in_expr(default)?;
            }
            if let Some(generated) = &mut column.generated {
                self.resolve_stored_sequence_references_in_expr(&mut generated.expression)?;
            }
        }
        Ok(Some(columns))
    }

    /// DEFAULT expression for `column` on `table`, when one was
    /// declared via `... <col> <type> DEFAULT <expr>`.
    pub fn column_default_expr(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Option<uqa_sql::ast::Expr>> {
        self.try_column_default_expr(table, column)
    }

    pub fn try_column_default_expr(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Option<uqa_sql::ast::Expr>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let cols = t.columns.read();
        let mut default = cols
            .iter()
            .find(|c| c.name == column)
            .and_then(|c| c.default.clone());
        drop(cols);
        if let Some(default) = &mut default {
            self.resolve_stored_sequence_references_in_expr(default)?;
        }
        Ok(default)
    }
}
