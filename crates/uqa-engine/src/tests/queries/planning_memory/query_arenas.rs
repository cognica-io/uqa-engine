//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::plan::{CommandPlan, RelationalPlan};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_mutation_query_arenas_admit_before_copying_and_retain_their_original_allowance(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("query-arena-ownership.db"));
    engine
        .sql("CREATE TABLE items(id integer PRIMARY KEY)", &[])
        .unwrap();
    let UnifiedPlan::Command(mut command) = UnifiedPlan::lower(uqa_sql::compile("MERGE INTO items AS target USING (VALUES(1)) AS source(id) ON target.id = source.id WHEN NOT MATCHED THEN INSERT(id) VALUES(source.id)").unwrap().remove(0)) else { panic!("command") };
    let CommandPlan::Merge(plan) = &mut *command else {
        panic!("merge")
    };
    let UnifiedPlan::Query(mut query) =
        UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    block.projections[0].expr = uqa_sql::ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
    plan.subqueries.push(*query);
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory();
    let before = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut outcome = None;
    let observed = allocation_counter::measure(|| {
        outcome = Some(
            uqa_execution::mutation::merge::analysis::merge_analysis_scope(&engine, plan, None),
        );
    });
    let outcome = outcome.unwrap();
    assert!(
        matches!(&outcome, Err(error) if error.sqlstate() == Some("53200")),
        "{provider:?}: scalar-query capture ignored its invoking allowance; {observed:?}"
    );
    assert!(
        observed.bytes_total < 256 << 10,
        "{provider:?}: {observed:?}"
    );
    drop(outcome);
    assert_eq!(memory.used(), before + occupied.bytes());
    drop(occupied);
    let scope = uqa_execution::mutation::merge::analysis::merge_analysis_scope(&engine, plan, None)
        .unwrap();
    assert!(memory.used() >= before + (4 << 20));
    let allocated = memory.used();
    let mut sibling = None;
    let observed = allocation_counter::measure(|| {
        sibling = Some(scope.clone());
    });
    assert!(
        observed.bytes_total < 64 << 10,
        "{provider:?}: {observed:?}"
    );
    let sibling = sibling.unwrap();
    assert_eq!(
        scope.scalar_subqueries.as_ptr(),
        sibling.scalar_subqueries.as_ptr()
    );
    drop(scope);
    assert_eq!(memory.used(), allocated);
    drop(sibling);
    assert_eq!(memory.used(), before);
    engine
        .sql("INSERT INTO items VALUES(1); SELECT id FROM items", &[])
        .unwrap();
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_query_arena_namespaces_match_postgresql_across_queries_mutations_and_reopen(
    #[case] provider: Provider,
) {
    // PostgreSQL 18.4 independently returns the ordered rows below for the same nested arenas, CTE shadowing and view/mutation statements.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("query-arena-namespaces.db");
    let engine = provider.open(&path);
    engine.sql("CREATE TABLE items(id integer PRIMARY KEY,n integer); INSERT INTO items VALUES(1,10),(2,20); CREATE VIEW item_view AS SELECT id,n AS amount FROM items", &[]).unwrap();
    for (sql, expected) in [
        ("SELECT i.id,q.n,(SELECT (SELECT i.n+1)) AS nested FROM items i CROSS JOIN LATERAL (SELECT (SELECT i.n+2) AS n UNION ALL SELECT (SELECT i.n+3)) q ORDER BY i.id,q.n", vec![vec![1,12,11],vec![1,13,11],vec![2,22,21],vec![2,23,21]]),
        ("WITH q AS NOT MATERIALIZED (SELECT id,(SELECT n+4) AS n FROM items) SELECT (SELECT q.n),(WITH q AS (SELECT 91 AS n) SELECT (SELECT q.n) FROM q) FROM q ORDER BY q.id",vec![vec![14,91],vec![24,91]]),
        ("UPDATE item_view SET amount=(SELECT amount+5) WHERE id=(SELECT 1) RETURNING id,amount", vec![vec![1,15]]),
        ("DELETE FROM items WHERE id=(SELECT 2) RETURNING id,(SELECT 42)",vec![vec![2,42]]),
        ("INSERT INTO items VALUES((SELECT 3),(SELECT 30)) RETURNING id,(SELECT 44)",vec![vec![3,44]]),
        ("MERGE INTO items AS target USING (VALUES(1),(4)) AS source(id) ON target.id=source.id WHEN MATCHED THEN UPDATE SET n=(SELECT target.n+1) WHEN NOT MATCHED THEN INSERT(id,n) VALUES(source.id,(SELECT 40)) RETURNING target.id,target.n",vec![vec![1,16],vec![4,40]]),
    ] {
        let result = engine.sql(sql, &[]).unwrap();
        assert_eq!(result.rows.len(),expected.len(), "{provider:?}: {sql}");
        for (row, values) in expected.iter().enumerate() {
            for (column,value) in values.iter().enumerate() {
                assert_eq!(result.value_at(row,column),Some(&Value::Int(*value)),"{provider:?}: {sql}");
            }
        }
    }
    check_rows(&engine);
    engine.close().unwrap();
    drop(engine);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        check_rows(&reopened);
        reopened.close().unwrap();
    }
}
fn check_rows(engine: &Engine) {
    let result = engine
        .sql("SELECT id,(SELECT amount) FROM item_view ORDER BY id", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 3);
    for (row, (id, value)) in [(1, 16), (3, 30), (4, 40)].into_iter().enumerate() {
        assert_eq!(result.value_at(row, 0), Some(&Value::Int(id)));
        assert_eq!(result.value_at(row, 1), Some(&Value::Int(value)));
    }
}
