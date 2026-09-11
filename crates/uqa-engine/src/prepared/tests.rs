//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::Engine;

#[test]
fn prepared_costs_distinguish_rare_and_unknown_index_keys() {
    let engine = Engine::new();
    for sql in [
        "CREATE TABLE cost_distribution (bucket integer)",
        "INSERT INTO cost_distribution SELECT CASE WHEN n=10000 THEN 2 ELSE 1 END FROM generate_series(1,10000) g(n)",
        "CREATE INDEX cost_bucket ON cost_distribution (bucket)",
        "ANALYZE cost_distribution",
        "PREPARE cost_query(integer) AS SELECT count(*) FROM cost_distribution WHERE bucket=$1",
        "EXECUTE cost_query(2)",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    let entry = engine.session.prepared.read()["cost_query"].clone();
    let generic = crate::capabilities::statement_planning::optimize_engine_plan(
        &engine,
        (*entry.logical_plan).clone(),
    )
    .unwrap();
    let generic_cost =
        crate::capabilities::statement_planning::estimate_engine_plan(&engine, &generic).unwrap();
    let stats = engine.try_query_column_stats("cost_distribution").unwrap();
    assert!(
        entry.total_custom_cost < generic_cost.execution,
        "custom={} generic={generic_cost:?} statistics={stats:?} plan={generic:?}",
        entry.total_custom_cost
    );
}
