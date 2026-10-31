//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::ProductionControl, CancellationToken};
use uqa_sql::{
    ast::FunctionOrderSyntax, routines::inlining::RoutinePlanExpressions, ColumnType,
    FunctionTypeResolver, ScalarExpr,
};

fn call(engine: &Engine, name: &str, value: Value) -> ScalarExpr {
    let binding = engine
        .resolve_function_overload(name, None, &[None], &[Some(ColumnType::Text)], false)
        .unwrap()
        .unwrap()
        .binding;
    ScalarExpr::Func {
        name: name.into(),
        binding: Some(binding),
        args: vec![ScalarExpr::TypedLiteral {
            value,
            ty: "text".into(),
            bound_type: Some(ColumnType::Text),
            parameter_index: None,
            collation_oid: None,
            composite_source: None,
        }],
        distinct: false,
        order_by: Vec::new(),
        order_syntax: FunctionOrderSyntax::Ordinary,
        filter: None,
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_routine_fold_admits_its_input_and_retains_only_the_planning_result(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("routine-results.db"));
    engine.sql("CREATE FUNCTION sql_identity(x text) RETURNS text LANGUAGE sql IMMUTABLE AS 'SELECT x'; CREATE FUNCTION plpgsql_identity(x text) RETURNS text LANGUAGE plpgsql IMMUTABLE AS 'BEGIN RETURN x; END'", &[]).unwrap();
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory().child(1024 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let sibling = memory.reserve(512).unwrap();
    for name in ["sql_identity", "plpgsql_identity"] {
        let expression = call(&engine, name, Value::Str("routine payload".repeat(128)));
        let expected = engine.evaluate_constant_routine(&expression).unwrap();
        let output = engine
            .evaluate_constant_routine_with_control(&expression, &control)
            .unwrap();
        assert_eq!(*output, expected, "{provider:?}: {name}");
        assert_eq!(memory.used(), sibling.bytes() + output.reserved_bytes());
        drop(output);
        assert_eq!(memory.used(), sibling.bytes());

        let large = call(&engine, name, Value::Str("x".repeat(4 * 1024 * 1024)));
        let mut result = None;
        let allocations = allocation_counter::measure(|| {
            result = Some(engine.evaluate_constant_routine_with_control(&large, &control));
        });
        assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
        assert!(
            allocations.bytes_total < 64 * 1024,
            "{provider:?}: {allocations:?}"
        );
        assert_eq!(memory.used(), sibling.bytes());

        for cancellation in [&original, &invoking] {
            cancellation.cancel();
            assert_eq!(
                engine
                    .evaluate_constant_routine_with_control(&expression, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            cancellation.reset();
            assert_eq!(memory.used(), sibling.bytes());
        }
        assert_eq!(
            *engine
                .evaluate_constant_routine_with_control(&expression, &control)
                .unwrap(),
            expected
        );
        assert_eq!(memory.used(), sibling.bytes());
    }
    drop(sibling);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
