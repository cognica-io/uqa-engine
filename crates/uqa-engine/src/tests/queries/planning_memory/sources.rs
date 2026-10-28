//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_execution::query::cte::context::QueryOutputRewriter;
use uqa_sql::{plan::RelationalPlan, ScalarExpr};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_owned_source_optimization_preserves_input_allocations(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("source-ownership.db"));
    engine.sql("SELECT 1", &[]).unwrap();
    let UnifiedPlan::Query(mut query) =
        UnifiedPlan::lower(uqa_sql::compile("VALUES ($1)").unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::Values { rows, .. } = &mut query.root else {
        panic!("values")
    };
    rows.resize_with(8192, || vec![ScalarExpr::Param(1)]);
    let pointer = rows.as_ptr();
    let row_pointers: Vec<_> = rows.iter().map(Vec::as_ptr).collect();
    let retention = engine.query_retention_control().unwrap();
    let memory = retention.memory();
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - 64 * 1024).unwrap();
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(engine.optimize_retained_query(*query).unwrap());
    });
    assert!(
        allocation.bytes_total < 16 * 1024,
        "{provider:?}: {allocation:?}"
    );
    let output = output.unwrap();
    let RelationalPlan::Values { rows, .. } = &output.root else {
        panic!("values")
    };
    assert_eq!(rows.as_ptr(), pointer);
    assert_eq!(rows.len(), row_pointers.len());
    for (row, pointer) in rows.iter().zip(row_pointers) {
        assert_eq!(row.as_ptr(), pointer);
        assert_eq!(row, &[ScalarExpr::Param(1)]);
    }
    drop(output);
    drop(occupied);
    assert_eq!(memory.used(), baseline);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_borrowed_source_optimization_preserves_private_destinations(#[case] provider: Provider) {
    use uqa_execution::schema::ctas::TableAsQuerySource;
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("borrowed-source.db"));
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(
        uqa_sql::compile("VALUES ($1), ($2), ($3)")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    let RelationalPlan::Values { rows: original, .. } = &query.root else {
        panic!("values")
    };
    let mut output = TableAsQuerySource::optimize(&engine, &query).unwrap();
    let RelationalPlan::Values { rows, .. } = &mut output.root else {
        panic!("values")
    };
    assert_eq!(rows, original);
    assert_ne!(rows.as_ptr(), original.as_ptr());
    for (row, input) in rows.iter().zip(original) {
        assert_ne!(row.as_ptr(), input.as_ptr());
    }
    rows[0][0] = ScalarExpr::Literal(Value::Null);
    assert_eq!(original[0], [ScalarExpr::Param(1)]);
    drop(output);
    let output = TableAsQuerySource::optimize(&engine, &query).unwrap();
    let RelationalPlan::Values { rows, .. } = &output.root else {
        panic!("values")
    };
    assert_eq!(rows, original);
    engine.close().unwrap();
}
