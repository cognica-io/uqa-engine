//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    analysis::{AnalysisCatalog, TableDefinition, ViewDefinition},
    resolution::RelationNameResolution,
};
use crate::type_resolution::ResolvedFunctionOverload;
use std::sync::Arc;

pub(super) struct Catalog;

impl Catalog {
    pub(super) fn context() -> BindingContext<'static> {
        BindingContext {
            catalog: Arc::new(Self),
            ..crate::binding::fixture::empty_binding_context()
        }
    }
}

fn relation(name: &str) -> Option<u32> {
    match name.strip_prefix("public.").unwrap_or(name) {
        "base" => Some(101),
        "body_only" => Some(102),
        "inner_view" => Some(201),
        "outer_view" => Some(202),
        "materialized" => Some(203),
        "routine_view" => Some(204),
        _ => None,
    }
}

impl AnalysisCatalog for Catalog {
    fn relation_dependency(
        &self,
        _: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<u32>, SQLError> {
        Ok(relation(name))
    }

    fn table_resolved(
        &self,
        _: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<TableDefinition>, SQLError> {
        if !matches!(relation(name), Some(101 | 102)) {
            return Ok(None);
        }
        let crate::Statement::CreateTable(table) =
            crate::compile("CREATE TABLE base (id integer)")?.remove(0)
        else {
            unreachable!()
        };
        Ok(Some(crate::binding::fixture::table_definition(
            table.columns,
        )))
    }

    fn table_name_resolved(
        &self,
        _: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<String>, SQLError> {
        Ok(matches!(relation(name), Some(101 | 102))
            .then(|| format!("public.{}", name.strip_prefix("public.").unwrap_or(name))))
    }

    fn view_resolved(
        &self,
        _: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<ViewDefinition>, SQLError> {
        let (sql, materialized) = match relation(name) {
            Some(201) => ("SELECT id FROM base", false),
            Some(202) => ("SELECT id FROM inner_view", false),
            Some(203) => ("SELECT id FROM body_only", true),
            Some(204) => ("SELECT public.f(id) AS id FROM base", false),
            _ => return Ok(None),
        };
        let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(crate::compile(sql)?.remove(0))
        else {
            unreachable!()
        };
        query.rewrite_scalar_expressions(&mut |expression| {
            if let ScalarExpr::Func { name, binding, .. } = expression {
                if name == "public.f" {
                    *binding = Some(routine_binding("f", ColumnType::Integer, 7));
                }
            }
        });
        Ok(Some(ViewDefinition {
            query: *query,
            output_columns: Some(vec!["id".into()]),
            materialized,
            materialized_column_types: vec![Some(ColumnType::Integer)],
        }))
    }

    fn foreign_table_resolved(
        &self,
        _: &RelationNameResolution,
        _: &str,
    ) -> Result<Option<TableDefinition>, SQLError> {
        Ok(None)
    }
    fn sequence_exists(&self, _: &RelationNameResolution, _: &str) -> Result<bool, SQLError> {
        Ok(false)
    }
    fn virtual_relation_schema(
        &self,
        _: &RelationNameResolution,
        _: &str,
    ) -> Result<Option<Vec<(String, ColumnType)>>, SQLError> {
        Ok(None)
    }
    fn sql_functions(
        &self,
        _: &RelationNameResolution,
        _: &str,
    ) -> Result<Option<Vec<Arc<crate::routines::SQLUserFunction>>>, SQLError> {
        Ok(None)
    }
}

fn routine_binding(name: &str, argument: ColumnType, identity: u8) -> FunctionBinding {
    FunctionBinding {
        object_id: Some([identity; 16]),
        name: format!("public.{name}"),
        argument_types: vec![argument.catalog_name()],
        builtin: false,
        dispatch: None,
        invocation: None,
        composite_field: None,
        resolution_error: None,
    }
}

pub(super) struct Routines;

impl FunctionTypeResolver for Routines {
    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        names: &[Option<String>],
        types: &[Option<ColumnType>],
        variadic: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        self.resolve_function_overload(name, binding, names, types, variadic)
            .map(|selected| selected.map(|selected| selected.return_type))
    }

    fn resolve_function_overload(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        _: &[Option<String>],
        types: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        let name = name.strip_prefix("public.").unwrap_or(name);
        let (argument, identity) = match (name, types) {
            ("f", [Some(ColumnType::Integer)]) => (ColumnType::Integer, 7),
            ("f", [Some(ColumnType::Text)]) => (ColumnType::Text, 8),
            ("g", [Some(ColumnType::Integer)]) => (ColumnType::Integer, 9),
            _ => return Ok(None),
        };
        if binding.is_some_and(|binding| binding.object_id != Some([identity; 16])) {
            return Ok(None);
        }
        Ok(Some(ResolvedFunctionOverload {
            binding: routine_binding(name, argument, identity),
            return_type: ColumnType::Integer,
            exact_matches: 1,
            known_arguments: 1,
            preferred_matches: 0,
            precedes_pg_catalog: true,
        }))
    }
}

impl RoutineResolution for Routines {
    fn resolve_static_sql_function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        names: &[Option<String>],
        types: &[Option<ColumnType>],
        variadic: bool,
    ) -> Result<Option<Arc<crate::routines::SQLUserFunction>>, SQLError> {
        let Some(selected) =
            self.resolve_function_overload(name, binding, names, types, variadic)?
        else {
            return Ok(None);
        };
        let sql = format!("CREATE FUNCTION {}({}) RETURNS integer LANGUAGE SQL STABLE AS 'SELECT id FROM body_only'", selected.binding.name, selected.binding.argument_types[0]);
        let crate::Statement::CreateFunction(mut definition) = crate::compile(&sql)?.remove(0)
        else {
            unreachable!()
        };
        definition.object_id = selected.binding.object_id;
        Ok(Some(Arc::new(crate::routines::SQLUserFunction::new(
            *definition,
            crate::routines::RoutineBody::Source,
        ))))
    }
}

pub(super) struct Aliases;
impl crate::schema::dependencies::oid_alias::OidAliasInput for Aliases {
    fn resolve_oid_alias_input(&self, _: &ColumnType, name: &str) -> Result<Option<i64>, SQLError> {
        Ok(match name {
            "base" => Some(101),
            "array_only" => Some(301),
            "array_item" => Some(302),
            "sequence_input" => Some(303),
            _ => None,
        })
    }
}
