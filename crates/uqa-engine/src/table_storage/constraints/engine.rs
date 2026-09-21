//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    table_not_found, DocId, Engine, RelationIdentity, SQLError, StorageBackendError,
    StorageBackendResult, TableState,
};
use uqa_storage::document_store::identifiers::{
    load_legacy_document_id_watermark, restored_document_id_watermark, DocumentIdAllocator,
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
        self.with_catalog_read_snapshot(|engine| {
            Ok(engine
                .check_constraint_definitions_in_execution(table)?
                .into_iter()
                .map(|constraint| (constraint.name, constraint.expr))
                .collect())
        })
    }

    /// Snapshot of every CHECK constraint, including `PostgreSQL` 18 enforcement
    /// metadata.
    pub fn try_check_constraint_definitions(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<uqa_sql::ast::TableCheck>> {
        self.with_catalog_read_snapshot(|engine| {
            engine.check_constraint_definitions_in_execution(table)
        })
    }

    pub(crate) fn check_constraint_definitions_in_execution(
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
        self.with_catalog_read_snapshot(|engine| engine.foreign_keys_in_execution(table))
    }

    pub(crate) fn foreign_keys_in_execution(
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
        self.with_catalog_read_snapshot(|engine| engine.referrers_in_execution(table))
    }

    pub(crate) fn referrers_in_execution(
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
            for fk in self.foreign_keys_in_execution(&other)? {
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
        self.with_catalog_read_snapshot(|engine| engine.unique_columns_in_execution(table))
    }

    pub(crate) fn unique_columns_in_execution(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<String>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let auto_increment =
            uqa_sql::schema::constraint_views::auto_increment_columns(&t.columns.read());
        Ok(uqa_sql::schema::constraint_views::unique_scalar_columns(
            self.key_constraints_in_execution(table)?,
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
        self.with_catalog_read_snapshot(|engine| engine.key_constraints_in_execution(table))
    }

    pub(crate) fn key_constraints_in_execution(
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

    pub(crate) fn table_identifier_allocator(
        &self,
        state: &TableState,
    ) -> StorageBackendResult<DocumentIdAllocator<'_>> {
        let durable = (state.persistence != uqa_sql::ast::RelationPersistence::Temporary)
            .then(|| self.storage.backend.as_ref()?.identifier_allocator())
            .flatten();
        DocumentIdAllocator::new(durable, state.object_id(), state.storage_generation())
    }

    /// Durable providers reserve identities in their storage namespace; serialized providers retain candidate locks through publication.
    pub(crate) fn allocate_next_id(&self, table: &str) -> Result<u64, SQLError> {
        let t = self
            .try_table(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::Internal(format!("unknown table `{table}`")))?;
        let allocator = self.table_identifier_allocator(&t).map_err(|error| {
            uqa_execution::mutation::errors::identifier_storage_error(
                &format!("allocate document id for `{table}`"),
                &error,
            )
        })?;
        let next_candidate = || {
            allocator.allocate(&mut t.next_id.lock()).map_err(|error| {
                uqa_execution::mutation::errors::identifier_storage_error(
                    &format!("allocate document id for `{table}`"),
                    &error,
                )
            })
        };
        if allocator.is_durable()
            || self.storage.backend.is_none()
            || t.persistence == uqa_sql::ast::RelationPersistence::Temporary
        {
            return next_candidate();
        }
        uqa_execution::mutation::identity::reserve_document_id(
            next_candidate,
            |id| self.reserve_document_id_candidate(table, id),
            |id| self.document_identity_is_occupied(table, &t, id),
            |acquisition| self.rollback_row_lock_acquisition(acquisition),
        )
    }

    fn document_identity_is_occupied(
        &self,
        table: &str,
        state: &TableState,
        id: DocId,
    ) -> Result<bool, SQLError> {
        let contains = |store: &dyn uqa_storage::DocumentStore| {
            store.contains_doc_id(id).map_err(|error| {
                SQLError::Internal(format!("check document identity in `{table}`: {error}"))
            })
        };
        if contains(state.document_store.read().as_ref())? {
            return Ok(true);
        }
        let backend = self.storage.backend.as_ref().expect("persistent table");
        if !self.backend_transaction_is_deferred()
            || !backend
                .change_version_monitor_is_nonblocking()
                .map_err(|error| SQLError::Internal(error.to_string()))?
        {
            // A writer excludes other commits. A rollback-journal reader also prevents a commit while its physical snapshot remains pinned.
            return Ok(false);
        }
        let canonical = self
            .try_resolve_table_name(table)
            .map_err(|error| SQLError::Internal(error.to_string()))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let mut reader = self.storage.document_identity_reader.lock();
        if reader.is_none() {
            let session = self
                .storage
                .provider
                .as_ref()
                .map_or_else(
                    || backend.open_session(),
                    |provider| provider.open_session(),
                )
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "open committed reader for document identity in `{table}`: {error}"
                    ))
                })?;
            *reader = Some(session.backend);
        }
        contains(
            reader
                .as_ref()
                .expect("identity reader initialized")
                .document_store(&canonical)
                .as_ref(),
        )
    }

    /// Move the watermark past `doc_id` if needed (called after a manual
    /// id assignment so the next allocation does not collide).
    pub(crate) fn advance_next_id(&self, table: &str, doc_id: DocId) -> StorageBackendResult<()> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let mut next = t.next_id.lock();
        self.table_identifier_allocator(&t)?
            .observe(&mut next, doc_id)
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
        let mut next = t.next_id.lock();
        self.table_identifier_allocator(&t)?
            .persist(catalog.as_ref(), table, &mut next)
    }

    pub(crate) fn load_persisted_next_id(
        catalog: &dyn uqa_storage::CatalogFacade,
        table: &str,
    ) -> StorageBackendResult<Option<u128>> {
        load_legacy_document_id_watermark(catalog, table)
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
        let maximum = state.document_store.read().max_doc_id()?;
        let mut current = state.next_id.lock();
        *current = restored_document_id_watermark(maximum, persisted.or(Some(*current)));
        Ok(())
    }
}
