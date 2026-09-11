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
        self, ViewAlterAccess, ViewAlterCatalog, ViewAlterContext, ViewAlterPublication,
        ViewAlterTransactions, ViewAlterWrite, ViewRegistryWrite,
    },
};
use uqa_sql::{
    ast::{AlterViewStmt, RelationPersistence},
    SQLError,
};
use uqa_storage::{StorageBackendResult, ViewRow};

impl Engine {
    pub(crate) fn alter_view(&self, statement: &AlterViewStmt) -> Result<(), SQLError> {
        view_alteration::alter_view(self, statement)
    }
    fn view_alter_context(&self) -> ViewAlterContext<'_> {
        ViewAlterContext {
            names: self,
            catalog: self,
            access: self,
            locks: self,
            roles: self.role_transfer_context(),
            dependencies: self,
            publication: self,
            changes: self,
            rewrite: self.view_rewrite_context(),
            notices: self.query_runtime_view().notices,
        }
    }
}
impl ViewAlterTransactions for Engine {
    fn with_view_write(&self, write: ViewAlterWrite<'_>) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| write(&engine.view_alter_context()))
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
        self.storage
            .catalog
            .as_ref()
            .map_or(Ok(()), |catalog| catalog.save_view(row))
    }
    fn views_write(&self) -> ViewRegistryWrite<'_> {
        Box::new(self.durable.views.write())
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
