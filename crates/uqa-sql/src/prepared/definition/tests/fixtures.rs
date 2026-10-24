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
impl crate::prepared::utility::PreparedSessionMetadata for NoRoutines {
    fn show_result_schema(&self, _: &str) -> Result<crate::RowSchema, crate::SQLError> {
        panic!("query fixture does not read session utility descriptors");
    }
    fn portal_result_schema(&self, _: &str) -> Option<crate::RowSchema> {
        panic!("query fixture does not read portal descriptors");
    }
    fn prepared_result_schema(&self, _: &str) -> Option<crate::RowSchema> {
        panic!("query fixture does not read prepared utility descriptors");
    }
}
impl crate::catalog::analysis::AnalysisCatalog for NoRoutines {
    fn effective_search_path(
        &self,
        resolution: &crate::catalog::resolution::RelationNameResolution,
    ) -> Result<Option<crate::catalog::resolution::EffectiveSearchPath>, crate::SQLError> {
        let mut schemas = resolution.search_path.clone();
        if !schemas.iter().any(|name| name == "pg_catalog") {
            schemas.insert(0, "pg_catalog".into());
        }
        Ok(Some(crate::catalog::resolution::EffectiveSearchPath {
            schemas,
            creation_namespace: resolution.search_path.first().cloned(),
        }))
    }

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
impl crate::schema::dependencies::oid_alias::OidAliasInput for NoRoutines {
    fn resolve_oid_alias_input(
        &self,
        _: &crate::ast::ColumnType,
        _: &str,
    ) -> Result<Option<i64>, crate::SQLError> {
        Ok(None)
    }
}

impl crate::routines::declaration::RoutineTypeCatalog for NoRoutines {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<crate::ast::ColumnDef>>, String> {
        panic!("query fixture does not read procedure types");
    }
    fn resolve_catalog_column_type(&self, _: &str) -> Option<crate::ColumnType> {
        panic!("query fixture does not read procedure types");
    }
    fn resolve_catalog_column_type_name(
        &self,
        _: &str,
    ) -> Result<crate::ColumnType, crate::SQLError> {
        panic!("query fixture does not read procedure types");
    }
    fn resolve_catalog_user_type_by_oid(&self, _: u32) -> Option<crate::ColumnType> {
        panic!("query fixture does not read procedure types");
    }
    fn require_type_usage(&self, _: &crate::ColumnType) -> Result<(), crate::SQLError> {
        panic!("query fixture does not read procedure types");
    }
    fn format_type(&self, _: &crate::ColumnType) -> Result<String, crate::SQLError> {
        panic!("query fixture does not read procedure types");
    }
}
impl crate::routines::resolution::RoutineOverloadCatalog for NoRoutines {
    fn routine_type_snapshot(&self) -> crate::routines::resolution::RoutineTypeSnapshot {
        panic!("query fixture does not resolve procedures");
    }
    fn routine_search_path(&self) -> Vec<String> {
        panic!("query fixture does not resolve procedures");
    }
    fn has_registered_scalar_function(&self, _: &str) -> bool {
        panic!("query fixture does not resolve procedures");
    }
    fn lookup_sql_routine_candidates(
        &self,
        _: &str,
    ) -> Result<Option<Vec<std::sync::Arc<crate::routines::SQLUserFunction>>>, crate::SQLError>
    {
        panic!("query fixture does not resolve procedures");
    }
    fn lookup_bound_sql_routine_candidates_by_binding(
        &self,
        _: &crate::ast::FunctionBinding,
    ) -> Option<Vec<std::sync::Arc<crate::routines::SQLUserFunction>>> {
        panic!("query fixture does not resolve procedures");
    }
    fn lookup_bound_sql_functions_by_binding(
        &self,
        _: &crate::ast::FunctionBinding,
    ) -> Option<Vec<std::sync::Arc<crate::routines::SQLUserFunction>>> {
        panic!("query fixture does not resolve procedures");
    }
}
