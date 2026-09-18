//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind explicit relation locking to live catalogs and transaction-owned lock marks.

use crate::Engine;
use uqa_execution::{
    row_locks::{
        binding::{RelationDefinitionSession, RelationLockCatalog, RelationLockSession},
        shared_objects::{SharedCatalogLock, SharedObjectLockSession},
        RelationLockMode, ScopedRelationLock,
    },
    statement::table_locks::{
        TableLockCatalog, TableLockContext, TableLockMetadata, TableLockSession,
    },
};
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::{
    catalog::{resolution::RelationResolution, stored_view::StoredView},
    SQLError,
};

impl Engine {
    pub(crate) fn table_lock_context(&self) -> TableLockContext<'_> {
        TableLockContext {
            catalog: self,
            roles: self,
            session: self,
        }
    }
}

impl TableLockCatalog for Engine {
    fn resolve(&self, name: &str, bound: bool) -> Result<RelationResolution, SQLError> {
        if bound {
            self.resolve_bound_relation_kind(name)
        } else {
            self.resolve_visible_relation_kind(name)
        }
    }

    fn table(&self, name: &str) -> Result<Option<TableLockMetadata>, SQLError> {
        let relation =
            uqa_core::RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        Ok(self
            .storage
            .tables
            .read()
            .get(&relation)
            .map(|table| TableLockMetadata {
                object_id: table.object_id(),
                security: table.security(),
            }))
    }

    fn view(&self, name: &str) -> Result<Option<StoredView>, SQLError> {
        let relation =
            uqa_core::RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        Ok(self.durable.views.read().get(&relation).cloned())
    }

    fn descendants(&self, name: &str) -> Result<Vec<String>, SQLError> {
        self.hierarchy_scan_tables(name, true)
    }
}

impl TableLockSession for Engine {
    fn in_transaction_block(&self) -> bool {
        Engine::in_transaction_block(self)
    }
    fn current_role(&self) -> RoleReference {
        self.current_role()
    }
}

impl RelationLockSession for Engine {
    fn acquire(
        &self,
        name: &str,
        mode: RelationLockMode,
        nowait: bool,
    ) -> Result<Option<ScopedRelationLock<'_>>, SQLError> {
        self.prepare_transaction_lock_wait()?;
        let table = self.row_locks.table_key(name);
        let marks = self.temporary_relation_lock_marks()?;
        if nowait {
            self.row_locks.try_acquire_scoped_relation(
                self.session_id,
                table,
                mode,
                marks,
                &self.runtime.cancellation,
            )
        } else {
            self.row_locks
                .acquire_scoped_relation(
                    self.session_id,
                    table,
                    mode,
                    marks,
                    &self.runtime.cancellation,
                )
                .map(Some)
        }
    }
    fn refresh_after_wait(&self) -> Result<(), SQLError> {
        self.refresh_explicit_statement_snapshot()
    }
}

impl RelationDefinitionSession for Engine {
    fn prepare_definition_write(&self) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer().map(|_| ())
    }
}

impl SharedObjectLockSession for Engine {
    fn acquire_shared_catalog(
        &self,
        target: SharedCatalogLock<'_>,
        mode: RelationLockMode,
    ) -> Result<ScopedRelationLock<'_>, SQLError> {
        self.prepare_transaction_lock_wait()?;
        let key = self.row_locks.shared_catalog_key(target);
        let marks = self.temporary_relation_lock_marks()?;
        self.row_locks.acquire_scoped_relation(
            self.session_id,
            key,
            mode,
            marks,
            &self.runtime.cancellation,
        )
    }

    fn refresh_shared_catalog(&self) -> Result<(), SQLError> {
        self.refresh_explicit_statement_snapshot()
    }
}

impl RelationLockCatalog for Engine {
    fn relation_object_id(&self, name: &str) -> Result<Option<[u8; 16]>, SQLError> {
        let relation =
            uqa_core::RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        if let Some(system) = uqa_sql::catalog::SystemRelation::at(&relation.schema, &relation.name)
        {
            return Ok(Some(system.object_id()));
        }
        if let Some(table) = self.storage.tables.read().get(&relation) {
            return Ok(Some(table.object_id()));
        }
        if let Some(view) = self.durable.views.read().get(&relation) {
            return Ok(Some(view.object_id));
        }
        if let Some(object_id) = self.durable.sequence_object_ids.read().get(&relation) {
            return Ok(Some(*object_id));
        }
        Ok(self
            .durable
            .foreign_tables
            .read()
            .get(&relation)
            .map(|table| table.object_id))
    }
    fn table_name(&self, object_id: [u8; 16]) -> Option<String> {
        self.storage.tables.read().iter().find_map(|(name, table)| {
            (table.object_id() == object_id).then(|| name.qualified_name())
        })
    }
}
