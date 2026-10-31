//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind explicit relation locking to live catalogs and transaction-owned lock marks.

use crate::Engine;
use uqa_core::memory::{Produced, ProductionControl};
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

    fn table(
        &self,
        name: &str,
        _roles: &std::collections::BTreeMap<String, uqa_sql::catalog::roles::RoleDefinition>,
    ) -> Result<Option<TableLockMetadata>, SQLError> {
        let relation =
            uqa_core::RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        self.storage
            .tables
            .read()
            .get(&relation)
            .map(|table| {
                Ok(TableLockMetadata {
                    object_id: table.object_id(),
                    security: table.security(),
                })
            })
            .transpose()
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
        let relation =
            uqa_core::RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        let identity = self
            .durable
            .catalog_indexes
            .read()
            .get(&relation)
            .map(crate::catalog_indexes::index_definition)
            .transpose()
            .map_err(|error| {
                uqa_sql::catalog::errors::storage_error("index lock identity", &error)
            })?
            .and_then(|definition| definition.catalog.map(|catalog| catalog.identity.object_id));
        let table = identity.map_or_else(
            || self.row_locks.table_key(name),
            |identity| self.row_locks.index_key(identity),
        );
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

    fn next_catalog_oid(&self) -> Result<u32, SQLError> {
        let durable = self
            .storage
            .backend
            .as_ref()
            .and_then(|backend| backend.identifier_allocator());
        self.row_locks.catalog_oids().next_oid(durable, || {
            let catalog = self.catalog_execution();
            uqa_execution::catalog::projection::largest_catalog_oid(
                &catalog.catalog_read_view(),
                &catalog.session.relation_name_resolution(),
            )
        })
    }
}

impl RelationLockCatalog for Engine {
    fn relation_object_id(&self, name: &str) -> Result<Option<[u8; 16]>, SQLError> {
        self.relation_object_id_with_control(name, &ProductionControl::uncontrolled())
    }
    fn relation_object_id_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<[u8; 16]>, SQLError> {
        control.check()?;
        let reference = uqa_core::RelationIdentity::try_borrowed_reference(name).map_err(
            |error| match control.format(format_args!("{error}")) {
                Ok(message) => SQLError::Internal(message.into_parts().0),
                Err(error) => error.into(),
            },
        )?;
        let relation =
            reference.identity_with_control(uqa_core::RelationName::decoded("public"), control)?;
        if let Some(system) = uqa_sql::catalog::SystemRelation::at(&relation.schema, &relation.name)
        {
            return Ok(Some(system.object_id()));
        }
        if let Some(table) = self.storage.tables.read().get(&*relation) {
            return Ok(Some(table.object_id()));
        }
        if let Some(view) = self.durable.views.read().get(&*relation) {
            return Ok(Some(view.object_id));
        }
        if let Some(object_id) = self.durable.sequence_object_ids.read().get(&*relation) {
            return Ok(Some(*object_id));
        }
        if let Some(index) = self.durable.catalog_indexes.read().get(&*relation) {
            return crate::catalog_indexes::index_definition(index)
                .map(|definition| definition.catalog.map(|catalog| catalog.identity.object_id))
                .map_err(|error| {
                    uqa_sql::catalog::errors::storage_error("index lock identity", &error)
                });
        }
        Ok(self
            .durable
            .foreign_tables
            .read()
            .get(&*relation)
            .map(|table| table.object_id))
    }
    fn relation_catalog_oid(&self, name: &str) -> Result<Option<u32>, SQLError> {
        let relation =
            uqa_core::RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        if let Some(table) = self.storage.tables.read().get(&relation) {
            return Ok(Some(table.relation_oids().relation));
        }
        if let Some(view) = self.durable.views.read().get(&relation) {
            return Ok(Some(view.relation_oids().relation));
        }
        Ok(self
            .durable
            .foreign_tables
            .read()
            .get(&relation)
            .map(|table| table.relation_oids().relation))
    }
    fn table_name(&self, object_id: [u8; 16]) -> Option<String> {
        self.table_name_with_control(object_id, &ProductionControl::uncontrolled())
            .expect("ordinary table identity lookup")
            .map(|name| {
                name.into_uncontrolled()
                    .expect("ordinary table identity name")
            })
    }
    fn table_name_with_control(
        &self,
        object_id: [u8; 16],
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<String>>, SQLError> {
        control.check()?;
        for (name, table) in self.storage.tables.read().iter() {
            control.check()?;
            if table.object_id() == object_id {
                return Ok(Some(name.qualified_name_with_control(control)?));
            }
        }
        Ok(None)
    }
    fn relation_name(&self, object_id: [u8; 16]) -> Option<String> {
        self.table_name(object_id)
            .or_else(|| {
                self.durable.views.read().iter().find_map(|(name, view)| {
                    (view.object_id == object_id).then(|| name.qualified_name())
                })
            })
            .or_else(|| {
                self.durable
                    .sequence_object_ids
                    .read()
                    .iter()
                    .find_map(|(name, id)| (*id == object_id).then(|| name.qualified_name()))
            })
            .or_else(|| {
                self.durable
                    .foreign_tables
                    .read()
                    .iter()
                    .find_map(|(name, table)| {
                        (table.object_id == object_id).then(|| name.qualified_name())
                    })
            })
    }
}
