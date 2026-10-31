//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_execution::query::cte::context::QueryOutputRewriter;
use uqa_sql::{plan::source_projection::SourceProjection, RowSchema};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_output_demand_shares_request_allowance_and_recovers(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("output-demand.db"));
    engine.sql("SELECT 1", &[]).unwrap();
    let UnifiedPlan::Query(mut query) =
        UnifiedPlan::lower(uqa_sql::compile("SELECT 1 AS kept").unwrap().remove(0))
    else {
        panic!("query");
    };
    let uqa_planner::RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block");
    };
    block.projections.resize(1024, block.projections[0].clone());
    let before = serde_json::to_value(&query).unwrap();
    let schema = RowSchema::with_types(
        vec!["kept".into(); 1024],
        vec![Some(uqa_sql::ColumnType::Integer); 1024],
    );
    let projection = SourceProjection::retaining_all();
    let retention = engine.query_retention_control().unwrap();
    let memory = retention.memory();
    let runtime = engine.query_runtime_view();
    let control = uqa_core::memory::ProductionControl::new(
        memory,
        retention.cancellation(),
        runtime.cancellation,
    );
    let baseline = memory.used();
    let input = query.copy_for_rewrite(&control).unwrap();
    let occupied = memory.reserve(memory.available() - 128).unwrap();
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(engine.prune_owned_query_outputs(
            input,
            &[],
            &projection,
            &schema,
            &|_| false,
            &control,
        ));
    });
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert!(
        allocation.bytes_total < 8192,
        "{provider:?}: {allocation:?}"
    );
    assert_eq!(memory.used(), baseline + occupied.bytes());
    assert_eq!(serde_json::to_value(&query).unwrap(), before);
    drop(occupied);

    let input = query.copy_for_rewrite(&control).unwrap();
    engine.cancel();
    let error = engine
        .prune_owned_query_outputs(input, &[], &projection, &schema, &|_| false, &control)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(serde_json::to_value(&query).unwrap(), before);
    assert_eq!(memory.used(), baseline);
    engine.reset_cancellation();

    let input = query.copy_for_rewrite(&control).unwrap();
    let occupied = memory.reserve(memory.available() - 1024).unwrap();
    let output = engine
        .prune_owned_query_outputs(input, &[], &projection, &schema, &|_| false, &control)
        .unwrap();
    assert_eq!(
        memory.used(),
        baseline + occupied.bytes() + output.reserved_bytes()
    );
    assert_eq!(
        output.analyzed_output.as_ref().unwrap().columns(),
        schema.columns()
    );
    assert_eq!(serde_json::to_value(&output.root).unwrap(), before["root"]);
    drop(output);
    drop(occupied);
    drop(query);
    assert_eq!(memory.used(), baseline);
    assert_eq!(
        engine.sql("SELECT 7", &[]).unwrap().value_at(0, 0),
        Some(&Value::Int(7))
    );
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
