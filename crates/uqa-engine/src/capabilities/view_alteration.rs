//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind view alteration to the active transaction and retained view registry guards.
use crate::Engine;
use uqa_core::RelationIdentity;
use uqa_execution::{
    catalog::view::{StoredView, ViewPublication},
    schema::view_alteration::{
        ViewAlterAccess, ViewAlterCatalog, ViewAlterContext, ViewAlterPublication,
        ViewAlterTransactions, ViewAlterWrite,
    },
};
use uqa_sql::{ast::RelationPersistence, SQLError};
use uqa_storage::{StorageBackendResult, ViewRow};

impl Engine {
    pub(crate) fn view_alter_context(&self) -> ViewAlterContext<'_> {
        ViewAlterContext {
            schema_moves: self.relation_schema_context(),
            names: self,
            catalog: self,
            authority: self.table_privilege_context(),
            creation: self.relation_creation_context(),
            locks: self,
            roles: self.role_transfer_context(),
            dependencies: self,
            column_dependencies: self,
            publication: self,
            changes: self,
            rewrite: self.view_rewrite_context(),
            notices: self.query_runtime_view().notices,
        }
    }
}
impl uqa_execution::schema::view_alteration::ViewColumnRenameInputs for Engine {
    fn view_column_rename_context(
        &self,
    ) -> uqa_execution::schema::view_alteration::ViewColumnRenameContext<'_> {
        uqa_execution::schema::view_alteration::ViewColumnRenameContext {
            views: self.view_reference_context(),
            routines: self.routine_rewrite_context(),
            events: self.event_lifecycle_context(),
            values: self.composite_value_context(),
        }
    }
}
impl ViewAlterTransactions for Engine {
    fn with_view_write(&self, write: ViewAlterWrite<'_>) -> Result<(), SQLError> {
        self.with_implicit_definition_transaction(|engine| write(&engine.view_alter_context()))
    }
}
impl ViewAlterCatalog for Engine {
    fn view(&self, relation: &RelationIdentity) -> Option<StoredView> {
        self.durable.views.read().get(relation).cloned()
    }
    fn persistence(&self, relation: &RelationIdentity) -> Option<RelationPersistence> {
        self.durable
            .views
            .read()
            .get(relation)
            .map(|view| view.persistence)
    }
}
impl ViewAlterAccess for Engine {
    fn ensure_owner(&self, name: &str, view: &StoredView) -> Result<String, SQLError> {
        self.ensure_view_owner(name, view)
    }
}
impl ViewPublication for Engine {
    fn has_catalog(&self) -> bool {
        self.storage.catalog.is_some()
    }
    fn save_view(&self, row: &ViewRow) -> StorageBackendResult<()> {
        self.storage.catalog.as_ref().map_or(Ok(()), |catalog| {
            uqa_execution::catalog::definition_revision::publish_view(catalog.as_ref(), row)
        })
    }
    fn save_view_representation(&self, row: &ViewRow) -> StorageBackendResult<()> {
        self.storage
            .catalog
            .as_ref()
            .map_or(Ok(()), |catalog| catalog.save_view(row))
    }
}

impl ViewAlterPublication for Engine {
    fn persist_rename(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> StorageBackendResult<Option<bool>> {
        self.storage
            .catalog
            .as_ref()
            .map(|catalog| catalog.rename_view(from, to))
            .transpose()
    }
}
