//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    column_not_found, materialize_constraint_metadata, table_next_id_metadata_key, table_not_found,
    DocId, Engine, RelationIdentity, SQLError, StorageBackendError, StorageBackendResult,
    TableState,
};

impl Engine {
    /// Atomically replace the complete durable constraint state for one table.
    /// SQL DDL prepares and validates the candidate before calling this method;
    /// persistence is written before the in-memory catalog is published.
    pub(crate) fn replace_constraint_state(
        &self,
        table: &str,
        columns: Vec<uqa_sql::ast::ColumnDef>,
        constraints: uqa_sql::ast::TableConstraintSet,
    ) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(|engine| {
            engine.replace_constraint_state_inner(table, columns, constraints)
        })
    }

    fn replace_constraint_state_inner(
        &self,
        table: &str,
        mut columns: Vec<uqa_sql::ast::ColumnDef>,
        mut constraints: uqa_sql::ast::TableConstraintSet,
    ) -> StorageBackendResult<()> {
        let table_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| table_not_found(table))?;
        let state = self
            .try_table(&table_name)?
            .ok_or_else(|| table_not_found(&table_name))?;
        for column in &mut columns {
            if let Some(reference) = &mut column.references {
                reference.table = self.canonical_foreign_key_target(&reference.table)?;
            }
        }
        for foreign_key in &mut constraints.foreign_keys {
            foreign_key.ref_table = self.canonical_foreign_key_target(&foreign_key.ref_table)?;
        }
        let relation =
            RelationIdentity::from_legacy_name(&table_name).map_err(StorageBackendError::Other)?;
        materialize_constraint_metadata(&relation, &mut columns, &mut constraints)?;
        if self.is_persistent() {
            self.try_save_table_schema_with_components(
                &table_name,
                &state,
                &columns,
                &constraints,
            )?;
        }
        *state.columns.write() = columns;
        *state.table_checks.write() = constraints.checks;
        *state.foreign_keys.write() = constraints.foreign_keys;
        *state.key_constraints.write() = constraints.key_constraints;
        self.mark_column_stats_dirty(&table_name, &state)?;
        self.refresh_value_indexes_for_table(&table_name)?;
        Ok(())
    }

    pub fn set_column_default(
        &self,
        table: &str,
        column: &str,
        default: Option<uqa_sql::ast::Expr>,
    ) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.set_column_default_inner(table, column, default)
        })
    }

    pub(in crate::engine_table_storage) fn set_column_default_inner(
        &self,
        table: &str,
        column: &str,
        mut default: Option<uqa_sql::ast::Expr>,
    ) -> StorageBackendResult<bool> {
        let table_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| table_not_found(table))?;
        let t = self
            .try_table(&table_name)?
            .ok_or_else(|| table_not_found(&table_name))?;
        if let Some(default) = &mut default {
            self.bind_sequence_references_in_expr(default)?;
        }
        let mut columns = t.columns.write();
        let mut next = columns.clone();
        let col = next
            .iter_mut()
            .find(|col| col.name == column)
            .ok_or_else(|| column_not_found(&table_name, column))?;
        col.default = default;
        self.mark_column_stats_dirty(&table_name, &t)?;
        if self.is_persistent() {
            self.try_save_table_schema_with_columns(&table_name, &t, &next)?;
        }
        *columns = next;
        Ok(true)
    }

    pub(crate) fn set_column_generated(
        &self,
        table: &str,
        column: &str,
        generated: Option<uqa_sql::ast::GeneratedColumn>,
    ) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.set_column_generated_inner(table, column, generated)
        })
    }

    pub(in crate::engine_table_storage) fn set_column_generated_inner(
        &self,
        table: &str,
        column: &str,
        mut generated: Option<uqa_sql::ast::GeneratedColumn>,
    ) -> StorageBackendResult<bool> {
        let table_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| table_not_found(table))?;
        let t = self
            .try_table(&table_name)?
            .ok_or_else(|| table_not_found(&table_name))?;
        if let Some(generated) = &mut generated {
            self.bind_sequence_references_in_expr(&mut generated.expression)?;
        }
        let mut columns = t.columns.write();
        let mut next = columns.clone();
        let col = next
            .iter_mut()
            .find(|col| col.name == column)
            .ok_or_else(|| column_not_found(&table_name, column))?;
        col.generated = generated;
        self.mark_column_stats_dirty(&table_name, &t)?;
        if self.is_persistent() {
            self.try_save_table_schema_with_columns(&table_name, &t, &next)?;
        }
        *columns = next;
        Ok(true)
    }

    pub fn set_column_not_null(
        &self,
        table: &str,
        column: &str,
        not_null: bool,
    ) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.set_column_not_null_inner(table, column, not_null)
        })
    }

    pub(in crate::engine_table_storage) fn set_column_not_null_inner(
        &self,
        table: &str,
        column: &str,
        not_null: bool,
    ) -> StorageBackendResult<bool> {
        let table_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| table_not_found(table))?;
        let t = self
            .try_table(&table_name)?
            .ok_or_else(|| table_not_found(&table_name))?;
        let mut next = t.columns.read().clone();
        let col = next
            .iter_mut()
            .find(|col| col.name == column)
            .ok_or_else(|| column_not_found(&table_name, column))?;
        col.not_null = not_null;
        col.not_null_explicit = not_null;
        col.not_null_validated = true;
        col.not_null_no_inherit = false;
        if !not_null {
            col.not_null_name = None;
        }
        let mut constraints = uqa_sql::ast::TableConstraintSet {
            persistence: t.persistence,
            on_commit: t.on_commit,
            checks: t.table_checks.read().clone(),
            foreign_keys: t.foreign_keys.read().clone(),
            key_constraints: t.key_constraints.read().clone(),
            hierarchy: t.hierarchy.read().clone(),
        };
        let relation =
            RelationIdentity::from_legacy_name(&table_name).map_err(StorageBackendError::Other)?;
        materialize_constraint_metadata(&relation, &mut next, &mut constraints)?;
        self.mark_column_stats_dirty(&table_name, &t)?;
        if self.is_persistent() {
            self.try_save_table_schema_with_components(&table_name, &t, &next, &constraints)?;
        }
        *t.columns.write() = next;
        *t.table_checks.write() = constraints.checks;
        *t.foreign_keys.write() = constraints.foreign_keys;
        *t.key_constraints.write() = constraints.key_constraints;
        Ok(true)
    }

    pub fn set_column_type(
        &self,
        table: &str,
        column: &str,
        ty: &uqa_sql::ast::ColumnType,
    ) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.set_column_type_inner(table, column, ty)
        })
    }

    pub(in crate::engine_table_storage) fn set_column_type_inner(
        &self,
        table: &str,
        column: &str,
        ty: &uqa_sql::ast::ColumnType,
    ) -> StorageBackendResult<bool> {
        let table_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| table_not_found(table))?;
        let t = self
            .try_table(&table_name)?
            .ok_or_else(|| table_not_found(&table_name))?;
        let mut columns = t.columns.write();
        let mut next = columns.clone();
        let col = next
            .iter_mut()
            .find(|col| col.name == column)
            .ok_or_else(|| column_not_found(&table_name, column))?;
        col.ty.clone_from(ty);
        self.mark_column_stats_dirty(&table_name, &t)?;
        if self.is_persistent() {
            self.try_save_table_schema_with_columns(&table_name, &t, &next)?;
        }
        *columns = next;
        Ok(true)
    }

    /// Register table-level CHECK, FK, PRIMARY KEY, and UNIQUE constraints. Called by the
    /// SQL `CREATE TABLE` path after the columns are in place.
    pub fn register_table_constraints(
        &self,
        table: &str,
        checks: Vec<uqa_sql::ast::TableCheck>,
        foreign_keys: Vec<uqa_sql::ast::ForeignKey>,
        key_constraints: Vec<uqa_sql::ast::TableKeyConstraint>,
    ) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(|engine| {
            engine.register_table_constraints_inner(table, checks, foreign_keys, key_constraints)
        })
    }

    pub(in crate::engine_table_storage) fn register_table_constraints_inner(
        &self,
        table: &str,
        checks: Vec<uqa_sql::ast::TableCheck>,
        mut foreign_keys: Vec<uqa_sql::ast::ForeignKey>,
        key_constraints: Vec<uqa_sql::ast::TableKeyConstraint>,
    ) -> StorageBackendResult<()> {
        let Some(table_name) = self.try_resolve_table_name(table)? else {
            return Err(StorageBackendError::Other(format!(
                "unknown table `{table}` while registering constraints"
            )));
        };
        let Some(t) = self.try_table(&table_name)? else {
            return Err(StorageBackendError::Other(format!(
                "unknown table `{table_name}` while registering constraints"
            )));
        };
        for foreign_key in &mut foreign_keys {
            foreign_key.ref_table = self.canonical_foreign_key_target(&foreign_key.ref_table)?;
        }
        let mut constraints = uqa_sql::ast::TableConstraintSet {
            persistence: t.persistence,
            on_commit: t.on_commit,
            checks,
            foreign_keys,
            key_constraints,
            hierarchy: t.hierarchy.read().clone(),
        };
        let relation =
            RelationIdentity::from_legacy_name(&table_name).map_err(StorageBackendError::Other)?;
        let mut columns = t.columns.read().clone();
        materialize_constraint_metadata(&relation, &mut columns, &mut constraints)?;
        if self.is_persistent() {
            self.try_save_table_schema_with_components(&table_name, &t, &columns, &constraints)?;
        }
        *t.columns.write() = columns;
        *t.table_checks.write() = constraints.checks;
        *t.foreign_keys.write() = constraints.foreign_keys;
        *t.key_constraints.write() = constraints.key_constraints;
        Ok(())
    }

    /// Atomically replace the schema components that ALTER hierarchy actions
    /// may inherit. The candidate is fully named and persisted before the
    /// in-memory table becomes visible with its new edge.
    pub(crate) fn replace_table_hierarchy_components(
        &self,
        table: &str,
        mut columns: Vec<uqa_sql::ast::ColumnDef>,
        checks: Vec<uqa_sql::ast::TableCheck>,
        mut foreign_keys: Vec<uqa_sql::ast::ForeignKey>,
        key_constraints: Vec<uqa_sql::ast::TableKeyConstraint>,
        hierarchy: uqa_sql::ast::TableHierarchy,
    ) -> StorageBackendResult<()> {
        let table_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| table_not_found(table))?;
        let state = self
            .try_table(&table_name)?
            .ok_or_else(|| table_not_found(&table_name))?;
        for foreign_key in &mut foreign_keys {
            foreign_key.ref_table = self.canonical_foreign_key_target(&foreign_key.ref_table)?;
        }
        let mut constraints = uqa_sql::ast::TableConstraintSet {
            persistence: state.persistence,
            on_commit: state.on_commit,
            checks,
            foreign_keys,
            key_constraints,
            hierarchy,
        };
        let relation =
            RelationIdentity::from_legacy_name(&table_name).map_err(StorageBackendError::Other)?;
        materialize_constraint_metadata(&relation, &mut columns, &mut constraints)?;
        if self.is_persistent() {
            self.try_save_table_schema_with_components(
                &table_name,
                &state,
                &columns,
                &constraints,
            )?;
        }
        *state.columns.write() = columns;
        *state.table_checks.write() = constraints.checks;
        *state.foreign_keys.write() = constraints.foreign_keys;
        *state.key_constraints.write() = constraints.key_constraints;
        *state.hierarchy.write() = constraints.hierarchy;
        self.refresh_value_indexes_for_table(&table_name)?;
        Ok(())
    }

    /// Append one validated PRIMARY KEY or UNIQUE tuple without replacing the
    /// table's existing CHECK, FOREIGN KEY, or key constraints. SQL DDL owns
    /// validation of existing rows before calling this storage mutation.
    pub(crate) fn add_key_constraint(
        &self,
        table: &str,
        constraint: &uqa_sql::ast::TableKeyConstraint,
    ) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(|engine| {
            engine.add_key_constraint_inner(table, constraint)
        })
    }

    pub(in crate::engine_table_storage) fn add_key_constraint_inner(
        &self,
        table: &str,
        constraint: &uqa_sql::ast::TableKeyConstraint,
    ) -> StorageBackendResult<()> {
        let table_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| table_not_found(table))?;
        let t = self
            .try_table(&table_name)?
            .ok_or_else(|| table_not_found(&table_name))?;
        let mut key_constraints = t.key_constraints.read().clone();
        key_constraints.push(constraint.clone());
        let mut columns = t.columns.read().clone();
        if constraint.kind == uqa_sql::ast::TableKeyConstraintKind::PrimaryKey {
            for key_column in &constraint.columns {
                let column = columns
                    .iter_mut()
                    .find(|column| column.name == *key_column)
                    .ok_or_else(|| column_not_found(&table_name, key_column))?;
                column.not_null = true;
            }
        }
        let mut constraints = uqa_sql::ast::TableConstraintSet {
            persistence: t.persistence,
            on_commit: t.on_commit,
            checks: t.table_checks.read().clone(),
            foreign_keys: t.foreign_keys.read().clone(),
            key_constraints,
            hierarchy: t.hierarchy.read().clone(),
        };
        let relation =
            RelationIdentity::from_legacy_name(&table_name).map_err(StorageBackendError::Other)?;
        materialize_constraint_metadata(&relation, &mut columns, &mut constraints)?;
        if self.is_persistent() {
            self.try_save_table_schema_with_components(&table_name, &t, &columns, &constraints)?;
        }
        *t.columns.write() = columns;
        *t.table_checks.write() = constraints.checks;
        *t.foreign_keys.write() = constraints.foreign_keys;
        *t.key_constraints.write() = constraints.key_constraints;
        self.refresh_value_indexes_for_table(&table_name)?;
        Ok(())
    }

    /// Snapshot of every CHECK constraint that applies to `table`, merging the
    /// column-level CHECKs into the table-level list. Returns `(name, expr)`
    /// pairs for backward API compatibility; use
    /// [`Self::try_check_constraint_definitions`] when enforcement metadata is
    /// required.
    pub fn check_constraints(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<(Option<String>, uqa_sql::ast::Expr)>> {
        self.try_check_constraints(table)
    }

    pub fn try_check_constraints(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<(Option<String>, uqa_sql::ast::Expr)>> {
        Ok(self
            .try_check_constraint_definitions(table)?
            .into_iter()
            .map(|constraint| (constraint.name, constraint.expr))
            .collect())
    }

    /// Snapshot of every CHECK constraint, including `PostgreSQL` 18 enforcement
    /// metadata.
    pub fn try_check_constraint_definitions(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<uqa_sql::ast::TableCheck>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let mut out = Vec::new();
        for col in t.columns.read().iter() {
            if let Some(expr) = col.check.clone() {
                out.push(uqa_sql::ast::TableCheck {
                    name: col
                        .check_name
                        .clone()
                        .or_else(|| Some(format!("{}_check", col.name))),
                    expr,
                    enforced: col.check_enforced,
                    validated: col.check_validated,
                    no_inherit: col.check_no_inherit,
                    partition_constraint: None,
                });
            }
        }
        out.extend(t.table_checks.read().iter().cloned());
        Ok(out)
    }

    /// Snapshot of constraints declared at table scope, without lifting the
    /// column-level forms into the result. Catalog synthesis uses this together
    /// with the column definitions so every physical constraint is represented
    /// exactly once.
    pub(crate) fn try_declared_table_constraints(
        &self,
        table: &str,
    ) -> StorageBackendResult<uqa_sql::ast::TableConstraintSet> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let checks = t.table_checks.read().clone();
        let foreign_keys = t.foreign_keys.read().clone();
        let key_constraints = t.key_constraints.read().clone();
        let hierarchy = t.hierarchy.read().clone();
        Ok(uqa_sql::ast::TableConstraintSet {
            persistence: t.persistence,
            on_commit: t.on_commit,
            checks,
            foreign_keys,
            key_constraints,
            hierarchy,
        })
    }

    /// Snapshot of every FOREIGN KEY constraint that applies to
    /// `table`. Column-level `REFERENCES` are lifted to single-column
    /// `ForeignKey` entries.
    pub fn foreign_keys(&self, table: &str) -> StorageBackendResult<Vec<uqa_sql::ast::ForeignKey>> {
        self.try_foreign_keys(table)
    }

    pub fn try_foreign_keys(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<uqa_sql::ast::ForeignKey>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let mut out: Vec<uqa_sql::ast::ForeignKey> = t.foreign_keys.read().clone();
        for col in t.columns.read().iter() {
            if let Some(reference) = col.references.clone() {
                out.push(uqa_sql::ast::ForeignKey {
                    name: reference
                        .name
                        .clone()
                        .or_else(|| Some(format!("{}_fkey", col.name))),
                    object_id: reference.object_id,
                    local_columns: vec![col.name.clone()],
                    ref_table: reference.table,
                    ref_columns: reference.column.into_iter().collect(),
                    on_update: reference.on_update,
                    on_delete: reference.on_delete,
                    on_delete_set_columns: Vec::new(),
                    match_type: reference.match_type,
                    enforced: reference.enforced,
                    validated: reference.validated,
                    deferrable: reference.deferrable,
                    initially_deferred: reference.initially_deferred,
                    period: reference.period,
                });
            }
        }
        for foreign_key in &mut out {
            foreign_key.ref_table =
                self.canonical_stored_foreign_key_target(&foreign_key.ref_table)?;
        }
        Ok(out)
    }

    /// Tables that hold a FOREIGN KEY pointing at `table`. Used by
    /// DELETE / DROP CASCADE to refuse the operation when a referrer
    /// has at least one row matching the target value.
    pub fn referrers_to(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<(String, uqa_sql::ast::ForeignKey)>> {
        self.try_referrers_to(table)
    }

    pub fn try_referrers_to(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<(String, uqa_sql::ast::ForeignKey)>> {
        let table = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| table_not_found(table))?;
        let target = Self::resolved_relation_identity(&table)?;
        self.try_table(&table)?
            .ok_or_else(|| table_not_found(&table))?;
        let mut out: Vec<(String, uqa_sql::ast::ForeignKey)> = Vec::new();
        let names: Vec<String> = self
            .storage
            .tables
            .read()
            .keys()
            .map(RelationIdentity::qualified_name)
            .collect();
        for other in names {
            for fk in self.try_foreign_keys(&other)? {
                if fk.enforced && Self::foreign_key_targets(&fk, &target) {
                    out.push((other.clone(), fk));
                }
            }
        }
        Ok(out)
    }

    /// Names of columns with a `UNIQUE` or `PRIMARY KEY` constraint
    /// declared on the table. Auto-increment columns are excluded
    /// because the engine guarantees their uniqueness through the
    /// monotonic id watermark, so re-checking is redundant.
    pub fn unique_columns(&self, table: &str) -> StorageBackendResult<Vec<String>> {
        self.try_unique_columns(table)
    }

    pub fn try_unique_columns(&self, table: &str) -> StorageBackendResult<Vec<String>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let cols = t.columns.read();
        let auto_increment: std::collections::BTreeSet<String> = cols
            .iter()
            .filter(|column| column.auto_increment.is_some())
            .map(|column| column.name.clone())
            .collect();
        drop(cols);
        Ok(self
            .try_key_constraints(table)?
            .into_iter()
            .filter(|constraint| constraint.columns.len() == 1)
            .map(|constraint| constraint.columns[0].clone())
            .filter(|column| !auto_increment.contains(column))
            .collect())
    }

    /// Every PRIMARY KEY / UNIQUE tuple declared on `table`. Legacy
    /// column metadata is lifted into scalar constraints so pre-v16 and API-
    /// created tables retain their existing behavior.
    pub fn key_constraints(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<uqa_sql::ast::TableKeyConstraint>> {
        self.try_key_constraints(table)
    }

    pub fn try_key_constraints(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<uqa_sql::ast::TableKeyConstraint>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let mut constraints = t.key_constraints.read().clone();
        for column in t.columns.read().iter() {
            let kind = if column.primary_key {
                Some(uqa_sql::ast::TableKeyConstraintKind::PrimaryKey)
            } else if column.unique {
                Some(uqa_sql::ast::TableKeyConstraintKind::Unique)
            } else {
                None
            };
            let Some(kind) = kind else {
                continue;
            };
            if constraints.iter().any(|constraint| {
                constraint.kind == kind
                    && constraint.columns.as_slice() == std::slice::from_ref(&column.name)
            }) {
                continue;
            }
            constraints.push(uqa_sql::ast::TableKeyConstraint {
                name: None,
                kind,
                columns: vec![column.name.clone()],
                nulls_not_distinct: false,
                without_overlaps: false,
            });
        }
        Ok(constraints)
    }

    /// Allocate the next id from the per-table watermark, returning the
    /// allocated value. Updates the watermark in place.
    pub(crate) fn allocate_next_id(&self, table: &str) -> Result<u64, SQLError> {
        let t = self
            .try_table(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::Internal(format!("unknown table `{table}`")))?;
        let mut g = t.next_id.lock();
        let id = u64::try_from(*g).map_err(|_| {
            SQLError::Internal(format!(
                "document id space for table `{table}` is exhausted"
            ))
        })?;
        *g += 1;
        Ok(id)
    }

    /// Move the watermark past `doc_id` if needed (called after a manual
    /// id assignment so the next allocation does not collide).
    pub(crate) fn advance_next_id(&self, table: &str, doc_id: DocId) -> StorageBackendResult<()> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let mut g = t.next_id.lock();
        let next = u128::from(doc_id) + 1;
        if next > *g {
            *g = next;
        }
        Ok(())
    }

    pub(crate) fn persist_next_id(&self, table: &str) -> StorageBackendResult<()> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        if t.persistence == uqa_sql::ast::RelationPersistence::Temporary {
            return Ok(());
        }
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        let next_id = t.next_id.lock().to_string();
        catalog.set_metadata(&table_next_id_metadata_key(table), &next_id)
    }

    pub(crate) fn load_persisted_next_id(
        catalog: &dyn uqa_storage::CatalogFacade,
        table: &str,
    ) -> StorageBackendResult<Option<u128>> {
        let Some(value) = catalog.get_metadata(&table_next_id_metadata_key(table))? else {
            return Ok(None);
        };
        if value.is_empty() {
            return Ok(None);
        }
        value.parse::<u128>().map(Some).map_err(|error| {
            StorageBackendError::Other(format!(
                "invalid persisted next id for table `{table}`: {error}"
            ))
        })
    }

    pub(crate) fn refresh_table_next_id(
        &self,
        table: &str,
        state: &TableState,
    ) -> StorageBackendResult<()> {
        let persisted = if state.columns.read().iter().any(|column| {
            column
                .auto_increment
                .as_ref()
                .is_some_and(uqa_sql::ast::AutoIncrement::is_legacy)
        }) {
            self.storage
                .catalog
                .as_ref()
                .map(|catalog| Self::load_persisted_next_id(catalog.as_ref(), table))
                .transpose()?
                .flatten()
        } else {
            None
        };
        let physical = u128::from(state.document_store.read().max_doc_id()?) + 1;
        let mut current = state.next_id.lock();
        *current = persisted.map_or_else(
            || (*current).max(physical),
            |persisted| persisted.max(physical),
        );
        Ok(())
    }
}
