//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compose foreign definition publication from actual registries and native dependency contexts.
use crate::Engine;
use uqa_execution::schema::foreign_definitions::ForeignDefinitionContext;
impl Engine {
    pub(crate) fn foreign_definition_context(&self) -> ForeignDefinitionContext<'_> {
        ForeignDefinitionContext {
            registry: self,
            publication: self,
            catalog: self.storage.catalog.as_deref(),
            changes: self,
            views: self.view_reference_context(),
            events: self.event_lifecycle_context(),
        }
    }
}
