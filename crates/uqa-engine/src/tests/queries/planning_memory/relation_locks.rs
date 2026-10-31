//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::plan::{QueryPlan, RelationalPlan, SourcePlan};

fn query(sql: &str) -> Box<QueryPlan> {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    query
}

#[rstest::rstest]
#[case::memory_cte(Provider::Memory, "cte")]
#[case::memory_view(Provider::Memory, "view")]
#[case::sqlite_native_cte(Provider::SQLiteNative, "cte")]
#[case::sqlite_native_view(Provider::SQLiteNative, "view")]
#[case::sqlite_kv_cte(Provider::SQLiteKeyValue, "cte")]
#[case::sqlite_kv_view(Provider::SQLiteKeyValue, "view")]
#[case::redb_cte(Provider::Redb, "cte")]
#[case::redb_view(Provider::Redb, "view")]
fn native_relation_lock_discovery_borrows_cte_names_and_view_bodies(
    #[case] provider: Provider,
    #[case] input: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("relation-lock-discovery.db"));
    let payload = "x".repeat(4 << 20);
    let plan = if input == "cte" {
        let mut plan = query("WITH seed AS (SELECT 1 AS id) SELECT id FROM seed");
        plan.ctes[0].name.clone_from(&payload);
        let RelationalPlan::QueryBlock(block) = &mut plan.root else {
            panic!("block")
        };
        let Some(SourcePlan::Table { name, .. }) = &mut block.from else {
            panic!("source")
        };
        name.clone_from(&payload);
        plan
    } else {
        engine
            .sql(
                "CREATE TABLE lock_origin(id integer); INSERT INTO lock_origin VALUES(1)",
                &[],
            )
            .unwrap();
        engine.sql(&format!("CREATE VIEW lock_wide_view AS SELECT id, '{payload}'::text AS body FROM lock_origin"), &[]).unwrap();
        query("SELECT id FROM lock_wide_view")
    };
    engine.sql("BEGIN", &[]).unwrap();
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let context = engine.row_lock_context();
    let discover = || uqa_execution::query::locking::lock_query_relations(context, &plan);
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut result = None;
    let allocation = allocation_counter::measure(|| result = Some(discover()));
    assert!(
        allocation.bytes_total < 256 << 10,
        "{provider:?} {input}: {allocation:?}"
    );
    result.unwrap().unwrap();
    assert_eq!(memory.used(), baseline + occupied.bytes());
    drop(occupied);
    for token in [resources.cancellation(), context.cancellation] {
        token.cancel();
        assert_eq!(discover().unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), baseline);
        token.reset();
    }
    discover().unwrap();
    assert_eq!(memory.used(), baseline);
    engine.sql("ROLLBACK", &[]).unwrap();
    if input == "view" {
        assert_eq!(
            engine
                .sql("SELECT id FROM lock_wide_view", &[])
                .unwrap()
                .rows[0]["id"],
            Value::Int(1)
        );
    }
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_relation_lock_scope_admits_wide_cte_indexes(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("relation-lock-cte-index.db"));
    let mut plan = query("WITH seed AS (SELECT 1) SELECT 1");
    let template = plan.ctes.remove(0);
    for index in 0..4096 {
        let mut cte = template.clone();
        cte.name = format!("scope_{index:04}");
        plan.ctes.push(cte);
    }
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let baseline = memory.used();
    let context = engine.row_lock_context();
    let discover = || uqa_execution::query::locking::lock_query_relations(context, &plan);
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut result = None;
    let allocation = allocation_counter::measure(|| result = Some(discover()));
    assert!(
        allocation.bytes_total < 256 << 10,
        "{provider:?}: {allocation:?}"
    );
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert_eq!(memory.used(), baseline + occupied.bytes());
    drop(occupied);
    for token in [resources.cancellation(), context.cancellation] {
        token.cancel();
        assert_eq!(discover().unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), baseline);
        token.reset();
    }
    discover().unwrap();
    assert_eq!(memory.used(), baseline);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
