//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::FunctionBinding,
    routines::{
        declaration::RoutineTypeCatalog,
        resolution::{RoutineOverloadCatalog, RoutineTypeSnapshot},
        CompiledFunctionBody, RoutineBody, SQLUserFunction,
    },
};
use std::sync::Arc;

struct Catalog {
    routines: Vec<Arc<SQLUserFunction>>,
    search_path: Vec<String>,
}

impl Catalog {
    fn new(declarations: &[&str], search_path: &[&str]) -> Self {
        let routines = declarations
            .iter()
            .map(|sql| {
                let crate::Statement::CreateFunction(mut def) =
                    crate::compile(sql).unwrap().remove(0)
                else {
                    unreachable!()
                };
                def.object_id = Some([7; 16]);
                Arc::new(SQLUserFunction::new(
                    *def,
                    RoutineBody::Bound(Arc::new(CompiledFunctionBody::SQL(Vec::new()))),
                ))
            })
            .collect();
        Self {
            routines,
            search_path: search_path.iter().map(|name| (*name).into()).collect(),
        }
    }
}

impl RoutineTypeCatalog for Catalog {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<crate::ast::ColumnDef>>, String> {
        Ok(None)
    }

    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        ColumnType::from_sql_name(name).ok()
    }

    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        ColumnType::from_sql_name(name)
    }

    fn resolve_catalog_user_type_by_oid(&self, _: u32) -> Option<ColumnType> {
        None
    }

    fn require_type_usage(&self, _: &ColumnType) -> Result<(), SQLError> {
        Ok(())
    }

    fn format_type(&self, ty: &ColumnType) -> Result<String, SQLError> {
        Ok(ty.regtype_name())
    }
}

impl RoutineOverloadCatalog for Catalog {
    fn routine_type_snapshot(&self) -> RoutineTypeSnapshot {
        Arc::default()
    }

    fn routine_search_path(&self) -> Vec<String> {
        self.search_path.clone()
    }

    fn has_registered_scalar_function(&self, _: &str) -> bool {
        false
    }

    fn lookup_sql_routine_candidates(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        let selected = self
            .routines
            .iter()
            .filter(|routine| routine.def.name == name)
            .cloned()
            .collect::<Vec<_>>();
        Ok((!selected.is_empty()).then_some(selected))
    }

    fn lookup_bound_sql_routine_candidates_by_binding(
        &self,
        _: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        Some(self.routines.clone())
    }

    fn lookup_bound_sql_functions_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        self.lookup_bound_sql_routine_candidates_by_binding(binding)
    }
}

#[test]
fn procedure_inputs_and_descriptors_match_postgresql() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/prepared_call_oracle.expected.json"
    )))
    .unwrap();
    let cases = reference["cases"].as_array().unwrap();
    let declarations = cases
        .iter()
        .filter_map(|case| {
            case["sql"]
                .as_str()
                .filter(|sql| sql.starts_with("CREATE PROCEDURE"))
        })
        .collect::<Vec<_>>();
    let catalog = Catalog::new(&declarations, &["public"]);
    let scopes = Scopes::default();
    let context = PreparedDefinitionContext {
        types: &NoRoutines,
        routines: &NoRoutines,
        procedures: &catalog,
        scopes: &scopes,
        session: &NoRoutines,
        aliases: &NoRoutines,
    };
    for (index, case) in cases
        .iter()
        .enumerate()
        .filter(|(_, case)| case["action"] == "prepare")
    {
        let sql = case["sql"].as_str().unwrap();
        let result = analyze_definition(
            &context,
            UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0)),
            &[],
        );
        if case["result"]["status"] == 7 {
            let error = result.err().unwrap_or_else(|| panic!("accepted {sql}"));
            assert_eq!(
                error.sqlstate(),
                case["result"]["sqlstate"].as_str(),
                "{sql}"
            );
            assert_eq!(
                error.to_string(),
                case["result"]["message"].as_str().unwrap(),
                "{sql}"
            );
            continue;
        }
        let definition = result.unwrap_or_else(|error| panic!("{sql}: {error}"));
        let descriptor = &cases[index + 1];
        assert_eq!(descriptor["action"], "describe");
        let oid = |ty: &Option<ColumnType>| {
            crate::catalog::result_type::postgres_result_type(ty.as_ref().unwrap()).type_oid
        };
        assert_eq!(
            serde_json::json!(definition
                .parameter_types
                .iter()
                .map(oid)
                .collect::<Vec<_>>()),
            descriptor["result"]["parameters"],
            "{sql}"
        );
        let columns = definition
            .result_schema
            .as_ref()
            .map(|schema| {
                schema
                    .columns()
                    .iter()
                    .zip(schema.column_types())
                    .map(|(name, ty)| serde_json::json!([name, oid(ty)]))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        assert_eq!(
            serde_json::json!(columns),
            descriptor["result"]["columns"],
            "{sql}"
        );
        assert!(definition.dependencies.routines.contains(&[7; 16]));
    }
}
