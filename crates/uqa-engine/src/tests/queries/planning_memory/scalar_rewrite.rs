//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_planner::statement_planning::schema_expressions::optimize_catalog_scalar;
use uqa_sql::{ast::FunctionOrderSyntax, ScalarExpr};

fn threshold_call(threshold: f64, bytes: usize) -> ScalarExpr {
    ScalarExpr::Func {
        order_syntax: FunctionOrderSyntax::Ordinary,
        name: "knn_match".into(),
        binding: None,
        args: vec![
            ScalarExpr::Literal(Value::Str("field".into())),
            ScalarExpr::Literal(Value::Str("x".repeat(bytes))),
            ScalarExpr::Literal(Value::Float(threshold)),
            ScalarExpr::Column("runtime".into()),
        ],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}

fn input() -> ScalarExpr {
    ScalarExpr::And(vec![
        threshold_call(0.2, 4 * 1024 * 1024),
        threshold_call(0.8, 1),
    ])
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_scalar_merge_admits_scratch_and_moves_original_payload(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("scalar-rewrite.db"));
    engine.sql("SELECT 1", &[]).unwrap();
    let retention = engine.query_retention_control().unwrap();
    let memory = retention.memory();
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available()).unwrap();
    let mut expression = input();
    let mut result = None;
    let allocated = allocation_counter::measure(|| {
        result = Some(optimize_catalog_scalar(
            &engine.statement_planning_context(),
            &mut expression,
        ));
    });
    assert_eq!(
        result.unwrap().unwrap_err().sqlstate(),
        Some("53200"),
        "{provider:?}"
    );
    assert!(
        allocated.bytes_total < 32 * 1024,
        "{provider:?}: {allocated:?}"
    );
    assert_eq!(memory.used(), baseline + occupied.bytes());
    drop((expression, occupied));

    let mut expression = input();
    engine.cancel();
    assert_eq!(
        optimize_catalog_scalar(&engine.statement_planning_context(), &mut expression)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    drop(expression);
    assert_eq!(memory.used(), baseline);
    engine.reset_cancellation();

    let mut expression = input();
    let ScalarExpr::And(items) = &expression else {
        unreachable!()
    };
    let ScalarExpr::Func { args, .. } = &items[0] else {
        unreachable!()
    };
    let buffer = args.as_ptr();
    let ScalarExpr::Literal(Value::Str(text)) = &args[1] else {
        unreachable!()
    };
    let payload = text.as_ptr();
    let allocated = allocation_counter::measure(|| {
        optimize_catalog_scalar(&engine.statement_planning_context(), &mut expression).unwrap();
    });
    assert!(
        allocated.bytes_total < 32 * 1024,
        "{provider:?}: {allocated:?}"
    );
    let ScalarExpr::Func { args, .. } = &expression else {
        panic!("selected first call")
    };
    assert_eq!(args.as_ptr(), buffer);
    let ScalarExpr::Literal(Value::Str(text)) = &args[1] else {
        panic!("original payload")
    };
    assert_eq!(text.as_ptr(), payload);
    assert_eq!(text.len(), 4 * 1024 * 1024);
    assert_eq!(args[2], ScalarExpr::Literal(Value::Float(0.8)));
    assert_eq!(memory.used(), baseline);
    drop(expression);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

fn wide_boolean() -> ScalarExpr {
    ScalarExpr::And(vec![
        ScalarExpr::And(
            (0..4096)
                .map(|index| ScalarExpr::Column(format!("column_{index}").into()))
                .collect(),
        ),
        ScalarExpr::Column("tail".into()),
    ])
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_boolean_flattening_admits_scratch_and_recovers(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("boolean-rewrite.db"));
    engine.sql("SELECT 1", &[]).unwrap();
    let retention = engine.query_retention_control().unwrap();
    let memory = retention.memory();
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available()).unwrap();
    let mut expression = wide_boolean();
    let mut result = None;
    let allocated = allocation_counter::measure(|| {
        result = Some(optimize_catalog_scalar(
            &engine.statement_planning_context(),
            &mut expression,
        ));
    });
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert!(
        allocated.bytes_total < 16 * 1024,
        "{provider:?}: {allocated:?}"
    );
    assert_eq!(memory.used(), baseline + occupied.bytes());
    drop((expression, occupied));

    let mut expression = wide_boolean();
    engine.cancel();
    assert_eq!(
        optimize_catalog_scalar(&engine.statement_planning_context(), &mut expression)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    drop(expression);
    assert_eq!(memory.used(), baseline);
    engine.reset_cancellation();

    let mut expression = wide_boolean();
    optimize_catalog_scalar(&engine.statement_planning_context(), &mut expression).unwrap();
    let ScalarExpr::And(items) = &expression else {
        panic!("flattened conjunction")
    };
    assert_eq!(items.len(), 4097);
    for (index, item) in items[..4096].iter().enumerate() {
        assert_eq!(item, &ScalarExpr::Column(format!("column_{index}").into()));
    }
    assert_eq!(items.last(), Some(&ScalarExpr::Column("tail".into())));
    assert_eq!(memory.used(), baseline);
    drop(expression);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
