//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Connect view deletion to existing transaction, registry and event boundaries.
use crate::Engine;
use uqa_core::RelationIdentity;
use uqa_execution::schema::view_removal::{
    self,
    context::{
        ViewRemovalContext, ViewRemovalEvents, ViewRemovalNames, ViewRemovalPublication,
        ViewRemovalTransactions,
    },
};
use uqa_sql::SQLError;
use uqa_storage::StorageBackendResult;

impl Engine {
    fn view_removal_context(&self) -> ViewRemovalContext<'_> {
        ViewRemovalContext {
            registry: self,
            publication: self,
            changes: self,
            names: self,
            events: self,
            routines: self,
            dependencies: self.view_dependency_context(),
            ownership: self.view_ownership_context(),
        }
    }
    pub fn drop_view(&self, name: &str) -> Result<bool, SQLError> {
        view_removal::drop_view(self, name)
    }
    pub(crate) fn drop_views(
        &self,
        names: &[String],
        cascade: bool,
        kind: &str,
    ) -> Result<(), SQLError> {
        view_removal::drop_views(self, names, cascade, kind)
    }
    pub(crate) fn remaining_view_drop_targets(
        &self,
        names: &[String],
    ) -> Result<Vec<String>, SQLError> {
        view_removal::remaining_view_drop_targets(&self.view_removal_context(), names)
    }
    pub(crate) fn drop_views_depending_on_relations(
        &self,
        relations: &[String],
    ) -> StorageBackendResult<()> {
        view_removal::drop_views_depending_on_relations(&self.view_removal_context(), relations)
    }
    pub(crate) fn drop_views_inner(
        &self,
        names: &[String],
        check_authority: bool,
    ) -> Result<(), SQLError> {
        view_removal::drop_views_inner(&self.view_removal_context(), names, check_authority)
    }
    pub(crate) fn drop_temporary_views_depending_on_relation_inner(
        &self,
        canonical_name: &str,
    ) -> StorageBackendResult<()> {
        view_removal::drop_temporary_views_depending_on_relation_inner(
            &self.view_removal_context(),
            canonical_name,
        )
    }
}
impl ViewRemovalTransactions for Engine {
    fn with_view_removal<R>(
        &self,
        operation: impl FnOnce(&Self, &ViewRemovalContext<'_>) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        self.with_implicit_transaction(|engine| operation(engine, &engine.view_removal_context()))
    }
}
impl ViewRemovalNames for Engine {
    fn relation_kind(&self, name: &str) -> Result<Option<(String, &'static str)>, SQLError> {
        self.try_resolve_visible_relation_kind(name)
    }
}
impl ViewRemovalEvents for Engine {
    fn rules_depending_on_relations(
        &self,
        names: &[String],
    ) -> StorageBackendResult<Vec<(RelationIdentity, String)>> {
        Engine::rules_depending_on_relations(self, names)
    }
    fn drop_rules_depending_on_relations_inner(
        &self,
        names: &[String],
    ) -> StorageBackendResult<()> {
        Engine::drop_rules_depending_on_relations_inner(self, names)
    }
    fn drop_relation_events_inner(&self, relation: &RelationIdentity) -> StorageBackendResult<()> {
        Engine::drop_relation_events_inner(self, relation)
    }
}
impl ViewRemovalPublication for Engine {
    fn drop_view(&self, relation: &RelationIdentity) -> StorageBackendResult<Option<bool>> {
        self.storage
            .catalog
            .as_ref()
            .map(|catalog| catalog.drop_view(relation))
            .transpose()
    }
}
