//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply retained registries and fresh catalog analysis inputs for view reference publication.
use crate::Engine;
use uqa_core::RelationIdentity;
use uqa_execution::schema::view_references::{self, ViewReferenceContext};
use uqa_storage::StorageBackendResult;

impl Engine {
    pub(crate) fn view_reference_context(&self) -> ViewReferenceContext<'_> {
        ViewReferenceContext {
            registry: self,
            publication: self,
            changes: self,
            dependencies: self.view_dependency_context(),
            catalog: self.catalog_execution(),
        }
    }
    pub(crate) fn rewrite_view_relation_references(
        &self,
        replacements: &std::collections::BTreeMap<RelationIdentity, RelationIdentity>,
    ) -> StorageBackendResult<()> {
        view_references::rewrite_view_relation_references(
            &self.view_reference_context(),
            replacements,
        )
    }
    pub(crate) fn views_depending_on_column(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Vec<String>> {
        view_references::views_depending_on_column(&self.view_reference_context(), table, column)
    }
    pub(crate) fn rewrite_view_column_references(
        &self,
        table: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        view_references::rewrite_view_column_references(
            &self.view_reference_context(),
            table,
            from,
            to,
        )
    }
}
