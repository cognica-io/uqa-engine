//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{AlterRoutineKind, FromClause, FunctionParamMode, FunctionVolatility};

fn variadic_value(expression: &Expr) -> &Expr {
    crate::expr::variadic_argument_value(expression)
        .unwrap_or_else(|| panic!("expected VARIADIC marker, got {expression:?}"))
}

#[test]
fn variadic_declarations_compile_and_survive_serde() {
    let statement = first(
        "CREATE FUNCTION collect(VARIADIC items integer[]) RETURNS integer[] LANGUAGE sql AS $$ SELECT items $$",
    );
    let Statement::CreateFunction(function) = &statement else {
        panic!("expected CREATE FUNCTION");
    };
    assert_eq!(function.params.len(), 1);
    assert_eq!(function.params[0].mode, FunctionParamMode::Variadic);
    assert_eq!(function.params[0].type_name, "int4[]");
    assert_eq!(function.identity_arity(), 1);
    assert_eq!(function.call_arity(), 1);

    let encoded = serde_json::to_string(&statement).unwrap();
    let decoded: Statement = serde_json::from_str(&encoded).unwrap();
    let Statement::CreateFunction(decoded) = decoded else {
        panic!("expected CREATE FUNCTION after round trip");
    };
    assert_eq!(decoded.params[0].mode, FunctionParamMode::Variadic);
}

#[test]
fn explicit_variadic_marker_survives_scalar_from_and_call_lowering() {
    let Statement::Select(select) = first("SELECT f(VARIADIC ARRAY[1, 2])") else {
        panic!("expected SELECT");
    };
    let Expr::Func { args, .. } = &select.projections[0].expr else {
        panic!("expected scalar function");
    };
    assert!(matches!(variadic_value(&args[0]), Expr::Array(values) if values.len() == 2));

    let Statement::Select(select) = first("SELECT f(VARIADIC items => ARRAY[1, 2])") else {
        panic!("expected SELECT");
    };
    let Expr::Func { args, .. } = &select.projections[0].expr else {
        panic!("expected scalar function");
    };
    assert!(matches!(
        &args[0],
        Expr::Func { name, .. } if name == crate::expr::NAMED_ARG_FUNCTION
    ));
    assert!(matches!(variadic_value(&args[0]), Expr::Array(values) if values.len() == 2));

    let Statement::Select(select) = first("SELECT * FROM f(VARIADIC ARRAY[1, 2])") else {
        panic!("expected SELECT");
    };
    let Some(FromClause::Function { args, .. }) = &select.from else {
        panic!("expected FROM function");
    };
    assert!(matches!(variadic_value(&args[0]), Expr::Array(values) if values.len() == 2));

    let Statement::Call { args, .. } = first("CALL p(VARIADIC ARRAY[1, 2])") else {
        panic!("expected CALL");
    };
    assert!(matches!(variadic_value(&args[0]), Expr::Array(values) if values.len() == 2));
}

#[test]
fn explicit_variadic_call_marker_survives_serde() {
    let statement = first("SELECT f(VARIADIC ARRAY[1, 2])");
    let encoded = serde_json::to_string(&statement).unwrap();
    assert!(encoded.contains(crate::expr::VARIADIC_ARG_FUNCTION));
    let decoded: Statement = serde_json::from_str(&encoded).unwrap();
    let Statement::Select(select) = decoded else {
        panic!("expected SELECT after round trip");
    };
    let Expr::Func { args, .. } = &select.projections[0].expr else {
        panic!("expected scalar function after round trip");
    };
    assert!(matches!(variadic_value(&args[0]), Expr::Array(values) if values.len() == 2));
}

#[test]
fn alter_function_compiles_exact_identity_and_supported_attributes() {
    let statement =
        first("ALTER FUNCTION app.f(IN integer, OUT text, INOUT bigint) IMMUTABLE STRICT");
    let Statement::AlterRoutine(alter) = statement else {
        panic!("expected ALTER FUNCTION");
    };
    assert_eq!(alter.kind, AlterRoutineKind::Function);
    assert_eq!(alter.name, "app.f");
    assert_eq!(alter.arg_types.as_deref().unwrap(), ["int4", "int8"]);
    assert!(alter.arg_type_references.is_empty());
    assert_eq!(alter.volatility, Some(FunctionVolatility::Immutable));
    assert_eq!(alter.strict, Some(true));
}

#[test]
fn alter_procedure_and_routine_preserve_target_kind_and_null_input_action() {
    let Statement::AlterRoutine(procedure) =
        first("ALTER PROCEDURE app.p(IN integer, OUT text) STABLE CALLED ON NULL INPUT")
    else {
        panic!("expected ALTER PROCEDURE");
    };
    assert_eq!(procedure.kind, AlterRoutineKind::Procedure);
    assert_eq!(procedure.arg_types.as_deref().unwrap(), ["int4"]);
    assert_eq!(procedure.volatility, Some(FunctionVolatility::Stable));
    assert_eq!(procedure.strict, Some(false));

    let Statement::AlterRoutine(routine) = first("ALTER ROUTINE app.f(integer) VOLATILE") else {
        panic!("expected ALTER ROUTINE");
    };
    assert_eq!(routine.kind, AlterRoutineKind::Routine);
    assert_eq!(routine.volatility, Some(FunctionVolatility::Volatile));
    assert_eq!(routine.strict, None);
}

#[test]
fn alter_function_preserves_percent_type_identity_and_serde_defaults() {
    let statement = first("ALTER FUNCTION app.f(source.value%TYPE) STABLE");
    let Statement::AlterRoutine(alter) = &statement else {
        panic!("expected ALTER FUNCTION");
    };
    assert_eq!(alter.arg_types.as_deref().unwrap(), ["source.value%type"]);
    assert_eq!(alter.arg_type_references.len(), 1);
    let reference = alter.arg_type_references[0]
        .as_ref()
        .expect("%TYPE reference is retained");
    assert_eq!(reference.schema, None);
    assert_eq!(reference.relation, "source");
    assert_eq!(reference.column, "value");

    let decoded: Statement = serde_json::from_value(serde_json::to_value(&statement).unwrap())
        .expect("ALTER FUNCTION round trips");
    let Statement::AlterRoutine(decoded) = decoded else {
        panic!("expected ALTER FUNCTION after round trip");
    };
    assert_eq!(&decoded, alter);

    let mut encoded = serde_json::to_value(statement).unwrap();
    let fields = encoded["AlterRoutine"].as_object_mut().unwrap();
    fields.remove("arg_type_references");
    fields.remove("volatility");
    fields.remove("strict");
    let legacy: Statement = serde_json::from_value(encoded).unwrap();
    let Statement::AlterRoutine(legacy) = legacy else {
        panic!("expected ALTER FUNCTION");
    };
    assert!(legacy.arg_type_references.is_empty());
    assert_eq!(legacy.volatility, None);
    assert_eq!(legacy.strict, None);
}

#[test]
fn alter_function_rejects_every_unsupported_action_explicitly() {
    let error = compile("ALTER FUNCTION app.f(integer) SECURITY DEFINER").unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert!(error.to_string().contains("ALTER FUNCTION"));
    assert!(error.to_string().contains("action `security`"));
}

#[test]
fn alter_function_preserves_an_omitted_signature_for_unique_resolution() {
    let Statement::AlterRoutine(alter) = first("ALTER FUNCTION app.f IMMUTABLE") else {
        panic!("expected ALTER FUNCTION");
    };
    assert_eq!(alter.arg_types, None);
    assert!(alter.arg_type_references.is_empty());

    let Statement::AlterRoutine(zero_arity) = first("ALTER FUNCTION app.f() IMMUTABLE") else {
        panic!("expected ALTER FUNCTION");
    };
    assert_eq!(zero_arity.arg_types, Some(Vec::new()));
}
