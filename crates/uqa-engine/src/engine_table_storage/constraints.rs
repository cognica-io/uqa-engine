//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column constraint mutation, key and foreign-key metadata, and identifier allocation.

use super::{
    column_not_found, table_not_found, DocId, Engine, RelationIdentity, SQLError,
    StorageBackendError, StorageBackendResult,
};

impl Engine {
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

    pub(super) fn set_column_default_inner(
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

    pub(super) fn set_column_not_null_inner(
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
        let mut columns = t.columns.write();
        let mut next = columns.clone();
        let col = next
            .iter_mut()
            .find(|col| col.name == column)
            .ok_or_else(|| column_not_found(&table_name, column))?;
        col.not_null = not_null;
        self.mark_column_stats_dirty(&table_name, &t)?;
        if self.is_persistent() {
            self.try_save_table_schema_with_columns(&table_name, &t, &next)?;
        }
        *columns = next;
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

    pub(super) fn set_column_type_inner(
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

    pub(super) fn register_table_constraints_inner(
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
        let constraints = uqa_sql::ast::TableConstraintSet {
            checks,
            foreign_keys,
            key_constraints,
        };
        if self.is_persistent() {
            let columns = t.columns.read().clone();
            self.try_save_table_schema_with_components(&table_name, &t, &columns, &constraints)?;
        }
        *t.table_checks.write() = constraints.checks;
        *t.foreign_keys.write() = constraints.foreign_keys;
        *t.key_constraints.write() = constraints.key_constraints;
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

    pub(super) fn add_key_constraint_inner(
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
        let constraints = uqa_sql::ast::TableConstraintSet {
            checks: t.table_checks.read().clone(),
            foreign_keys: t.foreign_keys.read().clone(),
            key_constraints,
        };
        if self.is_persistent() {
            self.try_save_table_schema_with_components(&table_name, &t, &columns, &constraints)?;
        }
        *t.columns.write() = columns;
        *t.key_constraints.write() = constraints.key_constraints;
        self.refresh_value_indexes_for_table(&table_name)?;
        Ok(())
    }

    /// Snapshot of every CHECK constraint that applies to `table`,
    /// merging the column-level CHECKs into the table-level list.
    /// Returns `(name, expr)` pairs where `name` is the constraint
    /// name when one was supplied (synthesised as `<col>_check` for
    /// column-level constraints).
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
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let mut out: Vec<(Option<String>, uqa_sql::ast::Expr)> = Vec::new();
        for col in t.columns.read().iter() {
            if let Some(expr) = col.check.clone() {
                out.push((Some(format!("{}_check", col.name)), expr));
            }
        }
        for c in t.table_checks.read().iter() {
            out.push((c.name.clone(), c.expr.clone()));
        }
        Ok(out)
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
                    name: Some(format!("{}_fkey", col.name)),
                    local_columns: vec![col.name.clone()],
                    ref_table: reference.table,
                    ref_columns: vec![reference.column],
                    on_update: reference.on_update,
                    on_delete: reference.on_delete,
                    on_delete_set_columns: Vec::new(),
                    match_type: reference.match_type,
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
            .tables
            .read()
            .keys()
            .map(RelationIdentity::qualified_name)
            .collect();
        for other in names {
            for fk in self.try_foreign_keys(&other)? {
                if Self::foreign_key_targets(&fk, &target) {
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
            .filter(|column| column.auto_increment)
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
}
