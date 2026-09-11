//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

pub(super) struct NoRoutines;
impl crate::FunctionTypeResolver for NoRoutines {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&crate::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<crate::ColumnType>],
        _: bool,
    ) -> Result<Option<crate::ColumnType>, crate::SQLError> {
        Ok(None)
    }
}
impl crate::routines::RoutineResolution for NoRoutines {}
impl crate::catalog::analysis::AnalysisCatalog for NoRoutines {
    fn table_resolved(
        &self,
        _: &crate::catalog::resolution::RelationNameResolution,
        _: &str,
    ) -> Result<Option<crate::catalog::analysis::TableDefinition>, crate::SQLError> {
        Ok(None)
    }
    fn table_name_resolved(
        &self,
        _: &crate::catalog::resolution::RelationNameResolution,
        _: &str,
    ) -> Result<Option<String>, crate::SQLError> {
        Ok(None)
    }
    fn view_resolved(
        &self,
        _: &crate::catalog::resolution::RelationNameResolution,
        _: &str,
    ) -> Result<Option<crate::catalog::analysis::ViewDefinition>, crate::SQLError> {
        Ok(None)
    }
    fn foreign_table_resolved(
        &self,
        _: &crate::catalog::resolution::RelationNameResolution,
        _: &str,
    ) -> Result<Option<crate::catalog::analysis::TableDefinition>, crate::SQLError> {
        Ok(None)
    }
    fn sequence_exists(
        &self,
        _: &crate::catalog::resolution::RelationNameResolution,
        _: &str,
    ) -> Result<bool, crate::SQLError> {
        Ok(false)
    }
    fn virtual_relation_schema(
        &self,
        _: &crate::catalog::resolution::RelationNameResolution,
        _: &str,
    ) -> Result<Option<Vec<(String, crate::ColumnType)>>, crate::SQLError> {
        Ok(None)
    }
    fn sql_functions(
        &self,
        _: &crate::catalog::resolution::RelationNameResolution,
        _: &str,
    ) -> Result<Option<Vec<std::sync::Arc<crate::routines::SQLUserFunction>>>, crate::SQLError>
    {
        Ok(None)
    }
}

pub(super) fn binding_context() -> crate::binding::context::BindingContext<'static> {
    crate::binding::context::BindingContext {
        catalog: std::sync::Arc::new(NoRoutines),
        resolution: crate::catalog::resolution::RelationNameResolution {
            search_path: vec!["public".into()],
            temporary_schema: "pg_temp_1".into(),
            temporary_namespace_allocated: false,
            current_user: "uqa".into(),
            lookup_mode: crate::catalog::resolution::RelationLookupMode::Dynamic,
        },
        ctes: std::collections::BTreeMap::new(),
        deferred_ctes: std::collections::BTreeMap::new(),
        non_returning_ctes: std::collections::BTreeSet::new(),
        scalar_subqueries: &[],
    }
}
