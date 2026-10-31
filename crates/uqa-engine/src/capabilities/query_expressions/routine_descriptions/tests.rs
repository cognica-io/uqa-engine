//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{capabilities::query_scope, Engine};
use uqa_sql::{expr::EngineHook, ColumnType};

mod table {
    use super::*;
    use uqa_execution::query::routine_invocation::SQLRoutineInvoker;
    use uqa_execution::routines::invocation::RoutineProjectionResult;

    #[rstest::rstest]
    #[case::sql("sql", "SELECT a + 1 UNION ALL SELECT a + 2", "SELECT a + 10")]
    #[case::plpgsql(
        "plpgsql",
        "BEGIN RETURN NEXT a + 1; RETURN NEXT a + 2; END",
        "BEGIN RETURN NEXT a + 10; END"
    )]
    fn native_table_descriptions_follow_replacement_rollback_and_privileges(
        #[case] language: &str,
        #[case] original: &str,
        #[case] replacement: &str,
    ) {
        let engine = Engine::new();
        engine.sql(&format!("CREATE FUNCTION public.kept(a integer) RETURNS SETOF integer LANGUAGE {language} STRICT AS $${original}$$"), &[]).unwrap();
        let binding = binding(&engine);
        let ctes = query_scope::new_for_current_routine(&engine).unwrap();
        let hook = ScopedEngineHook::new(&engine, &ctes);
        let descriptions = RoutineCallDescriptions::default();
        let project = |value| match hook
            .invoke_projection_function(
                &binding.name,
                Some(&binding),
                &[(None, value)],
                &descriptions,
            )
            .unwrap()
        {
            RoutineProjectionResult::Table(result) => result.rows,
            RoutineProjectionResult::Scalar(_) => panic!("set routine"),
        };
        for value in 0..32 {
            assert_eq!(
                project(Value::Int(value)),
                vec![vec![Value::Int(value + 1)], vec![Value::Int(value + 2)]]
            );
        }
        let original_scope = hook.routine_description_scope().unwrap();
        assert!(descriptions.get(&binding, &original_scope).is_some());
        engine.sql(&format!("BEGIN; CREATE OR REPLACE FUNCTION public.kept(a integer) RETURNS SETOF integer LANGUAGE {language} STRICT AS $${replacement}$$"), &[]).unwrap();
        assert!(hook.routine_description_scope().is_none());
        assert_eq!(project(Value::Int(3)), vec![vec![Value::Int(13)]]);
        engine.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(
            project(Value::Int(3)),
            vec![vec![Value::Int(4)], vec![Value::Int(5)]]
        );
        engine.sql("CREATE ROLE table_reader; REVOKE EXECUTE ON FUNCTION public.kept(integer) FROM PUBLIC; SET ROLE table_reader", &[]).unwrap();
        let Err(uqa_execution::ExecError::SQL(error)) = hook.invoke_projection_function(
            &binding.name,
            Some(&binding),
            &[(None, Value::Null)],
            &descriptions,
        ) else {
            panic!("strict NULL must still check current EXECUTE privilege");
        };
        assert_eq!(error.sqlstate(), Some("42501"));
    }

    #[rstest::rstest]
    #[case::sql("sql", "SELECT a UNION ALL SELECT a + 1")]
    #[case::plpgsql("plpgsql", "BEGIN RETURN NEXT a; RETURN NEXT a + 1; END")]
    fn native_bound_table_descriptions_keep_current_values_and_release_allowance(
        #[case] language: &str,
        #[case] body: &str,
    ) {
        let engine = Engine::new();
        engine.sql(&format!("CREATE FUNCTION public.kept(a integer) RETURNS SETOF integer LANGUAGE {language} AS $${body}$$"), &[]).unwrap();
        let binding = binding(&engine);
        let ctes = query_scope::new_for_current_routine(&engine).unwrap();
        let hook = ScopedEngineHook::new(&engine, &ctes);
        let descriptions = RoutineCallDescriptions::default();
        let budget = uqa_core::memory::MemoryBudget::new(64 * 1024);
        let token = uqa_core::CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let scope = || hook.routine_description_scope();
        let context = engine.routine_invocation_context();
        for value in 0..32 {
            let result = invocation::call_bound_user_table_function_with_descriptions(
                &context,
                &binding,
                &[(None, Value::Int(value))],
                None,
                RoutineCallRetention {
                    descriptions: &descriptions,
                    scope: &scope,
                    control,
                },
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                result.rows,
                vec![vec![Value::Int(value)], vec![Value::Int(value + 1)]]
            );
        }
        assert!(descriptions.get(&binding, &scope().unwrap()).is_some());
        assert!(budget.used() > 0 && budget.used() <= budget.limit());
        token.cancel();
        assert_eq!(
            invocation::call_bound_user_table_function_with_descriptions(
                &context,
                &binding,
                &[(None, Value::Int(40))],
                None,
                RoutineCallRetention {
                    descriptions: &descriptions,
                    scope: &scope,
                    control
                }
            )
            .unwrap()
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        drop(descriptions);
        assert_eq!(budget.used(), 0);
    }
}

fn binding(engine: &Engine) -> FunctionBinding {
    engine
        .routine_invocation_context()
        .overloads
        .resolve_static_sql_function_match(
            "public.kept",
            None,
            &[None],
            &[Some(ColumnType::Integer)],
            false,
        )
        .unwrap()
        .unwrap()
        .binding()
}

#[rstest::rstest]
#[case::plpgsql("plpgsql", "BEGIN RETURN a + 1; END", "BEGIN RETURN a + 2; END")]
#[case::sql("sql", "SELECT a + 1", "SELECT a + 2")]
fn native_routine_descriptions_follow_replacement_and_rollback(
    #[case] language: &str,
    #[case] original_body: &str,
    #[case] replacement_body: &str,
) {
    let engine = Engine::new();
    engine.sql(&format!("CREATE FUNCTION public.kept(a integer) RETURNS integer LANGUAGE {language} AS $${original_body}$$"), &[]).unwrap();
    let binding = binding(&engine);
    let ctes = query_scope::new_for_current_routine(&engine).unwrap();
    let hook = ScopedEngineHook::new(&engine, &ctes);
    let descriptions = RoutineCallDescriptions::default();
    let control = ProductionControl::uncontrolled();
    for value in 0..32 {
        assert_eq!(
            hook.call_bound_user_function_with_descriptions(
                &binding,
                &[(None, Value::Int(value))],
                &descriptions,
                &control
            )
            .unwrap()
            .unwrap(),
            Value::Int(value + 1)
        );
    }
    let original = hook.routine_description_scope().unwrap();
    assert!(
        descriptions.get(&binding, &original).is_some(),
        "native bindings must qualify for actual metadata reuse"
    );
    engine.sql(&format!("BEGIN; CREATE OR REPLACE FUNCTION public.kept(a integer) RETURNS integer LANGUAGE {language} AS $${replacement_body}$$"), &[]).unwrap();
    assert!(hook.routine_description_scope().is_none());
    assert_eq!(
        hook.call_bound_user_function_with_descriptions(
            &binding,
            &[(None, Value::Int(10))],
            &descriptions,
            &control
        )
        .unwrap()
        .unwrap(),
        Value::Int(12)
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        hook.call_bound_user_function_with_descriptions(
            &binding,
            &[(None, Value::Int(10))],
            &descriptions,
            &control
        )
        .unwrap()
        .unwrap(),
        Value::Int(11)
    );
    engine
        .sql("DROP FUNCTION public.kept(integer)", &[])
        .unwrap();
    assert_eq!(
        hook.call_bound_user_function_with_descriptions(
            &binding,
            &[(None, Value::Int(10))],
            &descriptions,
            &control
        )
        .unwrap()
        .unwrap_err()
        .sqlstate(),
        Some("42883")
    );
}

#[rstest::rstest]
#[case::plpgsql("plpgsql", "BEGIN RETURN a; END")]
#[case::sql("sql", "SELECT a")]
fn native_routine_descriptions_keep_execute_checks_for_strict_null_calls(
    #[case] language: &str,
    #[case] body: &str,
) {
    let engine = Engine::new();
    engine.sql(&format!("CREATE FUNCTION public.kept(a integer) RETURNS integer LANGUAGE {language} STRICT AS $${body}$$; CREATE ROLE routine_reader; REVOKE EXECUTE ON FUNCTION public.kept(integer) FROM PUBLIC"), &[]).unwrap();
    let binding = binding(&engine);
    let ctes = query_scope::new_for_current_routine(&engine).unwrap();
    let hook = ScopedEngineHook::new(&engine, &ctes);
    let descriptions = RoutineCallDescriptions::default();
    let control = ProductionControl::uncontrolled();
    assert_eq!(
        hook.call_bound_user_function_with_descriptions(
            &binding,
            &[(None, Value::Int(1))],
            &descriptions,
            &control
        )
        .unwrap()
        .unwrap(),
        Value::Int(1)
    );
    assert!(descriptions
        .get(&binding, &hook.routine_description_scope().unwrap())
        .is_some());
    engine.sql("SET ROLE routine_reader", &[]).unwrap();
    assert!(hook.routine_description_scope().is_none());
    assert_eq!(
        hook.call_bound_user_function_with_descriptions(
            &binding,
            &[(None, Value::Null)],
            &descriptions,
            &control
        )
        .unwrap()
        .unwrap_err()
        .sqlstate(),
        Some("42501")
    );
}
