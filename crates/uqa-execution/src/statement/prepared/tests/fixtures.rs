//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

pub(super) struct NoRoutines;
impl uqa_sql::FunctionTypeResolver for NoRoutines {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&uqa_sql::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<uqa_sql::ColumnType>],
        _: bool,
    ) -> Result<Option<uqa_sql::ColumnType>, uqa_sql::SQLError> {
        Ok(None)
    }
}
impl uqa_sql::routines::RoutineResolution for NoRoutines {}
impl uqa_sql::catalog::analysis::AnalysisCatalog for NoRoutines {
    fn table_resolved(
        &self,
        _: &uqa_sql::catalog::resolution::RelationNameResolution,
        _: &str,
    ) -> Result<Option<uqa_sql::catalog::analysis::TableDefinition>, uqa_sql::SQLError> {
        Ok(None)
    }
    fn table_name_resolved(
        &self,
        _: &uqa_sql::catalog::resolution::RelationNameResolution,
        _: &str,
    ) -> Result<Option<String>, uqa_sql::SQLError> {
        Ok(None)
    }
    fn view_resolved(
        &self,
        _: &uqa_sql::catalog::resolution::RelationNameResolution,
        _: &str,
    ) -> Result<Option<uqa_sql::catalog::analysis::ViewDefinition>, uqa_sql::SQLError> {
        Ok(None)
    }
    fn foreign_table_resolved(
        &self,
        _: &uqa_sql::catalog::resolution::RelationNameResolution,
        _: &str,
    ) -> Result<Option<uqa_sql::catalog::analysis::TableDefinition>, uqa_sql::SQLError> {
        Ok(None)
    }
    fn sequence_exists(
        &self,
        _: &uqa_sql::catalog::resolution::RelationNameResolution,
        _: &str,
    ) -> Result<bool, uqa_sql::SQLError> {
        Ok(false)
    }
    fn virtual_relation_schema(
        &self,
        _: &uqa_sql::catalog::resolution::RelationNameResolution,
        _: &str,
    ) -> Result<Option<Vec<(String, uqa_sql::ColumnType)>>, uqa_sql::SQLError> {
        Ok(None)
    }
    fn sql_functions(
        &self,
        _: &uqa_sql::catalog::resolution::RelationNameResolution,
        _: &str,
    ) -> Result<Option<Vec<std::sync::Arc<uqa_sql::routines::SQLUserFunction>>>, uqa_sql::SQLError>
    {
        Ok(None)
    }
}

pub(super) fn binding_context() -> uqa_sql::binding::context::BindingContext<'static> {
    uqa_sql::binding::context::BindingContext {
        catalog: std::sync::Arc::new(NoRoutines),
        resolution: uqa_sql::catalog::resolution::RelationNameResolution {
            search_path: vec!["public".into()],
            temporary_schema: "pg_temp_1".into(),
            temporary_namespace_allocated: false,
            current_user: "uqa".into(),
            lookup_mode: uqa_sql::catalog::resolution::RelationLookupMode::Dynamic,
        },
        ctes: std::collections::BTreeMap::new(),
        deferred_ctes: std::collections::BTreeMap::new(),
        non_returning_ctes: std::collections::BTreeSet::new(),
        scalar_subqueries: &[],
    }
}
