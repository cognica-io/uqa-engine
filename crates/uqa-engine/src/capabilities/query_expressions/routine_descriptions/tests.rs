//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{capabilities::query_scope, Engine};
use uqa_sql::{expr::EngineHook, ColumnType};

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
    let ctes = query_scope::new_for_current_routine(&engine);
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
    let ctes = query_scope::new_for_current_routine(&engine);
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
