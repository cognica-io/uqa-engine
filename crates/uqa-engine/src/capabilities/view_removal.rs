//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Connect view removal to existing transaction, registry and event boundaries.
use crate::Engine;
use uqa_core::RelationIdentity;
use uqa_execution::schema::view_removal::context::{
    ViewRemovalContext, ViewRemovalEvents, ViewRemovalPublication,
};
use uqa_sql::SQLError;
use uqa_storage::StorageBackendResult;

impl Engine {
    pub(crate) fn view_removal_context(&self) -> ViewRemovalContext<'_> {
        ViewRemovalContext {
            registry: self,
            publication: self,
            changes: self,
            events: self,
            ownership: self.view_ownership_context(),
        }
    }
    pub fn drop_view(&self, name: &str) -> Result<bool, SQLError> {
        self.with_implicit_definition_transaction(|engine| {
            uqa_execution::schema::removal::direct::drop_view(
                &engine.relation_removal_context(),
                name,
            )
        })
    }
}
impl ViewRemovalEvents for Engine {
    fn drop_relation_events_inner(&self, relation: &RelationIdentity) -> StorageBackendResult<()> {
        self.event_lifecycle_context()
            .drop_relation_events_inner(relation)
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
