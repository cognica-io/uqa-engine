//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::routines::{
    declaration::RoutineTypeCatalog,
    resolution::{RoutineOverloadCatalog, RoutineOverloadContext, RoutineTypeSnapshot},
    RoutineBody, SQLUserFunction,
};
use std::sync::Arc;

struct Procedures(Vec<Arc<SQLUserFunction>>);
impl Procedures {
    fn new() -> Self {
        let crate::Statement::CreateFunction(mut definition) = crate::compile(
            "CREATE PROCEDURE public.prepare_call(v integer) LANGUAGE plpgsql AS $$BEGIN NULL; END$$",
        ).unwrap().remove(0) else { panic!("procedure declaration") };
        definition.object_id = Some([23; 16]);
        Self(vec![Arc::new(SQLUserFunction::new(
            *definition,
            RoutineBody::Source,
        ))])
    }
}
impl RoutineTypeCatalog for Procedures {
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
impl RoutineOverloadCatalog for Procedures {
    fn routine_type_snapshot(&self) -> RoutineTypeSnapshot {
        Arc::default()
    }
    fn routine_search_path(&self) -> Vec<String> {
        vec!["public".into()]
    }
    fn has_registered_scalar_function(&self, _: &str) -> bool {
        false
    }
    fn lookup_sql_routine_candidates(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        Ok((name == "prepare_call" || name == "public.prepare_call").then(|| self.0.clone()))
    }
    fn lookup_bound_sql_routine_candidates_by_binding(
        &self,
        _: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        Some(self.0.clone())
    }
    fn lookup_bound_sql_functions_by_binding(
        &self,
        _: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        None
    }
}

#[test]
fn procedural_call_preparation_reads_selected_unknown_inputs_before_cache_publication() {
    let procedures = Procedures::new();
    let scopes = Scopes::default();
    let context = StatementAnalysisContext {
        scopes: &scopes,
        routines: &NoRoutines,
        aliases: &NoRoutines,
    };
    let overloads = RoutineOverloadContext {
        catalog: &procedures,
    };
    for (sql, state) in [
        ("CALL prepare_call('bad')", "22P02"),
        ("CALL absent_call('bad')", "42883"),
    ] {
        for _ in 0..2 {
            let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
            let error = analyze_procedural_plan(&context, &overloads, &procedures, &mut plan, &[])
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
        }
    }
    let mut plan = UnifiedPlan::lower(
        crate::compile("CALL prepare_call(v => '42')")
            .unwrap()
            .remove(0),
    );
    let analysis =
        analyze_procedural_plan(&context, &overloads, &procedures, &mut plan, &[]).unwrap();
    assert_eq!(analysis.result, AnalyzedResult::Command);
    assert_eq!(analysis.dependencies.routines, [[23; 16]].into());
    let UnifiedPlan::Command(command) = &plan else {
        panic!("command")
    };
    let CommandPlan::Call { args, .. } = command.as_ref() else {
        panic!("CALL")
    };
    let (decoded, _) = crate::ir::analyze_expression_call_arguments(args).unwrap();
    assert!(matches!(
        decoded[0].value,
        ScalarExpr::TypedLiteral {
            value: Value::Int(42),
            bound_type: Some(ColumnType::Integer),
            ..
        }
    ));
    let retained = serde_json::to_value(&plan).unwrap();
    analyze_procedural_plan(&context, &overloads, &procedures, &mut plan, &[]).unwrap();
    assert_eq!(serde_json::to_value(&plan).unwrap(), retained);
}
