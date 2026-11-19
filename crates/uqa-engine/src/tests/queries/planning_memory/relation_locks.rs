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

fn pause_automatic_statistics(engine: &Engine) {
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, std::sync::atomic::Ordering::Release);
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
    // Measure borrowing from the same catalog view. The refresh fixture below explicitly commits independent statistics before discovering locks.
    pause_automatic_statistics(&engine);
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
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn relation_lock_discovery_observes_committed_statistics_before_borrowing(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("relation-lock-refresh.db"));
    pause_automatic_statistics(&engine);
    engine
        .sql(
            "CREATE TABLE lock_origin(id integer); INSERT INTO lock_origin VALUES(1)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            &format!(
                "CREATE VIEW lock_wide_view AS SELECT id, '{}'::text AS body FROM lock_origin",
                "x".repeat(4 << 20)
            ),
            &[],
        )
        .unwrap();
    let plan = query("SELECT id FROM lock_wide_view");
    engine.sql("BEGIN", &[]).unwrap();
    let backend = engine.storage.backend.as_ref().unwrap();
    let previous = backend.read_view_revision().unwrap().unwrap();
    let peer = engine.new_session().unwrap();
    pause_automatic_statistics(&peer);
    peer.sql("ANALYZE lock_origin", &[]).unwrap();
    peer.close().unwrap();

    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let context = engine.row_lock_context();
    let discover = || uqa_execution::query::locking::lock_query_relations(context, &plan);
    // Refresh through the real locking boundary. Providers may decode their durable catalog here before borrowing from the new view.
    discover().unwrap();
    let current = backend.read_view_revision().unwrap().unwrap();
    assert!(!current.same_committed_state(&previous), "{provider:?}");
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut result = None;
    let allocation = allocation_counter::measure(|| result = Some(discover()));
    assert!(
        allocation.bytes_total < 256 << 10,
        "{provider:?}: {allocation:?}"
    );
    result.unwrap().unwrap();
    assert_eq!(memory.used(), baseline + occupied.bytes());
    drop(occupied);
    engine.sql("ROLLBACK", &[]).unwrap();
    let result = engine
        .sql("SELECT id, length(body) FROM lock_wide_view", &[])
        .unwrap();
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(result.value_at(0, 1), Some(&Value::Int(4 << 20)));
    drop(result);
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
