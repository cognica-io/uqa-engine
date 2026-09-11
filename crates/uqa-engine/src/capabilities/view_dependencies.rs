//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply live view catalogs and their existing publication services to dependency execution.

use crate::Engine;
use uqa_execution::schema::view_dependencies::{self, ViewDependencyContext};
use uqa_sql::{ast::FunctionBinding, SQLError};
use uqa_storage::StorageBackendResult;

impl Engine {
    fn view_dependency_context(&self) -> ViewDependencyContext<'_> {
        ViewDependencyContext {
            views: self,
            publication: self,
            changes: self,
        }
    }
    pub(crate) fn views_depending_on_relation(
        &self,
        canonical_name: &str,
    ) -> StorageBackendResult<Vec<String>> {
        view_dependencies::views_depending_on_relation(
            &self.view_dependency_context(),
            canonical_name,
        )
    }
    pub(crate) fn views_depending_on_sequence(
        &self,
        canonical_name: &str,
    ) -> StorageBackendResult<Vec<String>> {
        view_dependencies::views_depending_on_sequence(
            &self.view_dependency_context(),
            canonical_name,
        )
    }
    pub(crate) fn views_depending_on_function(
        &self,
        target: &FunctionBinding,
    ) -> StorageBackendResult<Vec<String>> {
        view_dependencies::views_depending_on_function(&self.view_dependency_context(), target)
    }
    pub(crate) fn cascade_view_closure(
        &self,
        initial: Vec<String>,
    ) -> Result<Vec<String>, SQLError> {
        view_dependencies::cascade_view_closure(&self.view_dependency_context(), initial)
    }
    pub(crate) fn rewrite_view_routine_identity(
        &self,
        target: &FunctionBinding,
        new_name: &str,
    ) -> StorageBackendResult<()> {
        view_dependencies::rewrite_view_routine_identity(
            &self.view_dependency_context(),
            target,
            new_name,
        )
    }
}
