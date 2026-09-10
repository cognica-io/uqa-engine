//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    table_next_id_metadata_key, table_not_found, DocId, Engine, RelationIdentity, SQLError,
    StorageBackendError, StorageBackendResult, TableState,
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

    pub(crate) fn set_column_default_inner(
        &self,
        table: &str,
        column: &str,
        default: Option<uqa_sql::ast::Expr>,
    ) -> StorageBackendResult<bool> {
        uqa_execution::schema::publication::columns::set_column_default(
            &self.schema_publication_context(),
            table,
            column,
            default,
        )
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

    pub(in crate::table_storage) fn set_column_not_null_inner(
        &self,
        table: &str,
        column: &str,
        not_null: bool,
    ) -> StorageBackendResult<bool> {
        uqa_execution::schema::publication::columns::set_column_not_null(
            &self.schema_publication_context(),
            table,
            column,
            not_null,
        )
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

    pub(in crate::table_storage) fn set_column_type_inner(
        &self,
        table: &str,
        column: &str,
        ty: &uqa_sql::ast::ColumnType,
    ) -> StorageBackendResult<bool> {
        uqa_execution::schema::publication::columns::set_column_type(
            &self.schema_publication_context(),
            table,
            column,
            ty,
        )
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

    pub(in crate::table_storage) fn register_table_constraints_inner(
        &self,
        table: &str,
        checks: Vec<uqa_sql::ast::TableCheck>,
        foreign_keys: Vec<uqa_sql::ast::ForeignKey>,
        key_constraints: Vec<uqa_sql::ast::TableKeyConstraint>,
    ) -> StorageBackendResult<()> {
        uqa_execution::schema::publication::columns::register_table_constraints(
            &self.schema_publication_context(),
            table,
            checks,
            foreign_keys,
            key_constraints,
        )
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
        let mut out = uqa_sql::schema::constraint_views::column_checks(&t.columns.read());
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
        let columns_declared = *t.columns_declared.read();
        Ok(uqa_sql::ast::TableConstraintSet {
            columns_declared: Some(columns_declared),
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
        uqa_sql::schema::constraint_views::append_column_foreign_keys(&t.columns.read(), &mut out);
        uqa_sql::schema::constraint_views::bind_stored_foreign_key_targets(self, &mut out)
            .map_err(StorageBackendError::Other)?;
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
        let auto_increment =
            uqa_sql::schema::constraint_views::auto_increment_columns(&t.columns.read());
        Ok(uqa_sql::schema::constraint_views::unique_scalar_columns(
            self.try_key_constraints(table)?,
            &auto_increment,
        ))
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
        uqa_sql::schema::constraint_views::append_column_keys(&t.columns.read(), &mut constraints);
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
