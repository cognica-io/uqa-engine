//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn captured(statement_parameter: usize, plan_parameter: usize) -> uqa_sql::routines::SQLRoutine {
    let mut definition = function();
    let statement = |parameter| Statement::Values {
        rows: vec![vec![Expr::Param(parameter)]],
    };
    definition.body = FunctionBody::Statements(vec![statement(statement_parameter)]);
    SQLUserFunction::new(
        definition,
        RoutineBody::Bound(
            CompiledFunctionBody::SQL(vec![UnifiedPlan::lower(statement(plan_parameter))]).shared(),
        ),
    )
    .shared()
}

fn check_routine(routine: uqa_sql::routines::SQLRoutine) -> Result<(), String> {
    let mut snapshot = empty_catalog().snapshot().clone();
    RetainedValue::make_mut(&mut snapshot.definitions.sql_user_functions)
        .insert("public.renamed".into(), vec![routine])
        .unwrap();
    check(&snapshot)
}

#[test]
fn native_routine_closure_rejects_parameter_ordinals_in_each_captured_owner() {
    check_routine(captured(1, 1)).unwrap();
    for (syntax, plan) in [(0, 1), (2, 1), (1, 0), (1, 2)] {
        let error = check_routine(captured(syntax, plan))
            .expect_err("an out-of-range routine parameter reached private activation");
        assert!(error.contains("routine"), "{error}");
    }
}

#[test]
fn native_routine_closure_rejects_incomplete_and_wrong_kind_bound_bodies() {
    for damage in 0..3 {
        let original = captured(1, 1);
        let body = match damage {
            0 => RoutineBody::Source,
            1 => RoutineBody::Bound(CompiledFunctionBody::SQL(Vec::new()).shared()),
            _ => RoutineBody::Bound(
                CompiledFunctionBody::SQL(vec![UnifiedPlan::lower(
                    uqa_sql::compile("CREATE TABLE unexpected(value integer)")
                        .unwrap()
                        .remove(0),
                )])
                .shared(),
            ),
        };
        let routine = SQLUserFunction::new(original.def.clone(), body).shared();
        check_routine(routine).expect_err("an invalid bound body reached private activation");
    }
}
