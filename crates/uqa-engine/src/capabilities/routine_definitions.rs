//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind routine definition analysis to current catalog data and compilation namespaces.

use crate::Engine;
use std::collections::BTreeSet;
use uqa_sql::{
    ast::{ColumnDef, ColumnType, CreateFunction, Statement},
    plan::QueryPlan,
    plpgsql::PlpgsqlCatalog,
    routines::{
        compilation::{RoutineCompilationContext, RoutineParserCatalog, RoutinePlanBinding},
        declaration::RoutineTypeCatalog,
        merge_columns::{self, StoredMergeColumnCatalog},
    },
    RowSchema, SQLError, SQLParam,
};

impl Engine {
    pub(crate) fn routine_compilation_context(&self) -> RoutineCompilationContext<'_> {
        RoutineCompilationContext {
            types: self,
            parsers: self,
            bindings: self,
            merge: self,
            regroles: self,
        }
    }
    pub(crate) fn bind_stored_merge_target_columns(
        &self,
        statement: &mut Statement,
    ) -> Result<bool, SQLError> {
        merge_columns::bind_stored_merge_target_columns(self, statement)
    }
}
impl RoutineTypeCatalog for Engine {
    fn try_describe_table(&self, name: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Engine::try_describe_table(self, name).map_err(|error| error.to_string())
    }
    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        uqa_execution::catalog::projection::resolve_catalog_column_type(
            &self.catalog_execution(),
            name,
        )
    }
    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        uqa_execution::catalog::projection::resolve_catalog_column_type_name(
            &self.catalog_execution(),
            name,
        )
    }
    fn resolve_catalog_domain_type_by_oid(&self, oid: u32) -> Option<ColumnType> {
        uqa_execution::catalog::projection::resolve_catalog_domain_type_by_oid(
            &self.catalog_execution(),
            oid,
        )
    }
}
impl StoredMergeColumnCatalog for Engine {
    fn stored_merge_target_definitions(&self, table: &str) -> Option<Vec<ColumnDef>> {
        self.table_entries()
            .into_iter()
            .find(|(name, _)| name == table)
            .map(|(_, table)| table.columns.read().clone())
    }
}
impl RoutineParserCatalog for Engine {
    fn plpgsql_catalog(&self) -> Result<PlpgsqlCatalog, SQLError> {
        uqa_execution::catalog::projection::plpgsql_catalog(&self.catalog_execution())
    }
}
impl RoutinePlanBinding for Engine {
    fn has_registered_aggregate_function(&self, name: &str) -> bool {
        Engine::has_registered_aggregate_function(self, name)
    }
    fn bind_definition_query_relations(&self, query: &mut QueryPlan) -> Result<(), SQLError> {
        self.bind_stored_query_relations(query, "SQL routine body", false)
            .map(|_| ())
    }
    fn bind_persisted_query_relations(&self, query: &mut QueryPlan) -> Result<(), SQLError> {
        self.bind_loaded_stored_query_relations(query, "SQL routine body", false)
            .map(|_| ())
    }
    fn bind_query_routines(
        &self,
        query: &mut QueryPlan,
        params: &[SQLParam],
        outer: &RowSchema,
    ) -> Result<RowSchema, SQLError> {
        let scope = super::query_scope::new_for_catalog_binding(self);
        uqa_execution::query::binding::bind_query_plan_routines_for_storage(
            self,
            query,
            params,
            &scope,
            Some(outer),
        )
    }
}

use uqa_core::RelationIdentity;
use uqa_execution::routines::{
    compilation::{self, RoutineCompilationSession, StoredRoutineCompilationContext},
    rewrites::{self, RoutineRewriteContext},
};
use uqa_sql::{ast::FunctionBinding, routines::CompiledFunctionBody};

impl Engine {
    pub(crate) fn stored_routine_compilation_context(&self) -> StoredRoutineCompilationContext<'_> {
        StoredRoutineCompilationContext {
            analysis: self.routine_compilation_context(),
            session: self,
        }
    }
    pub(crate) fn routine_rewrite_context(&self) -> RoutineRewriteContext<'_> {
        RoutineRewriteContext {
            registry: self,
            publication: self,
            compilation: self.stored_routine_compilation_context(),
            columns: self.stored_column_binding_context(),
            changes: self,
        }
    }
    pub(crate) fn compile_sql_function_body(
        &self,
        def: &CreateFunction,
    ) -> Result<CompiledFunctionBody, SQLError> {
        uqa_sql::routines::compilation::compile_function_body(
            &self.routine_compilation_context(),
            def,
        )
    }
    pub(crate) fn compile_persisted_sql_function(
        &self,
        def: &CreateFunction,
    ) -> Result<CompiledFunctionBody, SQLError> {
        compilation::compile_persisted_sql_function(&self.stored_routine_compilation_context(), def)
    }
    pub(crate) fn stored_merge_dependency_body(
        &self,
        def: &CreateFunction,
    ) -> Result<Option<CompiledFunctionBody>, SQLError> {
        compilation::stored_merge_dependency_body(&self.stored_routine_compilation_context(), def)
    }
    pub(crate) fn rewrite_routine_relation_references(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> Result<(), SQLError> {
        rewrites::rewrite_routine_relation_references(&self.routine_rewrite_context(), from, to)
    }
    pub(crate) fn rewrite_routine_column_references(
        &self,
        relation: &RelationIdentity,
        from: &str,
        to: &str,
    ) -> Result<(), SQLError> {
        rewrites::rewrite_routine_column_references(
            &self.routine_rewrite_context(),
            relation,
            from,
            to,
        )
    }
    pub(crate) fn publish_stored_routine_body_rewrites(
        &self,
        definitions: Vec<CreateFunction>,
    ) -> Result<(), SQLError> {
        rewrites::publish_stored_routine_body_rewrites(&self.routine_rewrite_context(), definitions)
    }
    pub(crate) fn refresh_stored_merge_target_plans(&self) -> Result<(), SQLError> {
        rewrites::refresh_stored_merge_target_plans(&self.routine_rewrite_context())
    }
    pub(crate) fn prepare_routine_column_alias_drop(
        &self,
        columns: BTreeSet<(String, String)>,
        removed: &[FunctionBinding],
    ) -> Result<Vec<CreateFunction>, SQLError> {
        uqa_execution::routines::removal::prepare_routine_column_alias_drop(
            &self.routine_removal_context(),
            columns,
            removed,
        )
    }
}
impl RoutineCompilationSession for Engine {
    fn replace_routine_search_path(&self, path: Vec<String>) -> Vec<String> {
        std::mem::replace(&mut self.session.state.write().search_path, path)
    }
    fn restore_routine_search_path(&self, path: Vec<String>) {
        self.session.state.write().search_path = path;
    }
}
