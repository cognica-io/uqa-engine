//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_execution::query::sources::build_join_operator_with_ctes;
use uqa_sql::plan::{RelationalPlan, RetainedCtePlan, SourcePlan};

fn query(sql: &str) -> Box<uqa_sql::plan::QueryPlan> {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    query
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_folded_sources_borrow_replaced_definitions_before_selected_execution(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("folded-source-owners.db"));
    let payload = "x".repeat(4 << 20);
    let mut input = query(&format!("WITH chosen AS NOT MATERIALIZED (SELECT '{payload}'::text AS payload) SELECT payload FROM chosen"));
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let runtime = engine.query_runtime_view();
    let control = ProductionControl::new(memory, resources.cancellation(), runtime.cancellation);
    let mut scope = crate::capabilities::query_scope::new_for_statement(&engine, None).unwrap();
    scope.insert_deferred(RetainedCtePlan::capture(&input.ctes.remove(0), &control).unwrap());
    let RelationalPlan::QueryBlock(block) = &mut input.root else {
        panic!("block")
    };
    let source = block.from.as_mut().unwrap();
    let SourcePlan::Table {
        executable_query, ..
    } = source
    else {
        panic!("table")
    };
    *executable_query = Some(query("SELECT 'selected'::text AS payload"));
    let context = engine.source_execution_context();
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (256 << 10)).unwrap();
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(build_join_operator_with_ctes(
            &context,
            source,
            &[],
            &mut scope,
            None,
            None,
        ));
    });
    assert!(
        allocation.bytes_total < 512 << 10,
        "{provider:?}: {allocation:?}"
    );
    let mut operator = result.unwrap().unwrap();
    operator.open().unwrap();
    let rows = operator.next().unwrap().unwrap().into_result_rows();
    assert_eq!(
        rows[0].values().next(),
        Some(&Value::Str("selected".into()))
    );
    operator.close().unwrap();
    drop((rows, operator, occupied));
    assert_eq!(memory.used(), baseline);
    for token in [resources.cancellation(), runtime.cancellation] {
        token.cancel();
        assert_eq!(
            build_join_operator_with_ctes(&context, source, &[], &mut scope, None, None)
                .err()
                .unwrap()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), baseline);
        token.reset();
    }
    let SourcePlan::Table {
        executable_query, ..
    } = source
    else {
        panic!("table")
    };
    *executable_query = None;
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(build_join_operator_with_ctes(
            &context,
            source,
            &[],
            &mut scope,
            None,
            None,
        ));
    });
    assert!(
        allocation.bytes_total < 256 << 10,
        "{provider:?}: {allocation:?}"
    );
    assert_eq!(result.unwrap().err().unwrap().sqlstate(), Some("53200"));
    drop(occupied);
    assert_eq!(memory.used(), baseline);
    let mut operator =
        build_join_operator_with_ctes(&context, source, &[], &mut scope, None, None).unwrap();
    assert!(
        memory.used() >= baseline + payload.len(),
        "{provider:?}: physical projection payload is not retained by its request: baseline={baseline}, used={}",
        memory.used()
    );
    operator.open().unwrap();
    let rows = operator.next().unwrap().unwrap().into_result_rows();
    assert!(matches!(rows[0].values().next(), Some(Value::Str(value)) if value == &payload));
    operator.close().unwrap();
    drop((rows, operator));
    assert_eq!(memory.used(), baseline);
    drop(scope);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_folded_sources_preserve_pg18_demand_lexical_scope_and_effects(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("folded-source-behavior.db");
    let engine = provider.open(&path);
    engine.sql("CREATE SCHEMA folded_reference; SET search_path=folded_reference; CREATE TABLE items(id integer); INSERT INTO items VALUES(1),(2),(3); CREATE SEQUENCE effect", &[]).unwrap();
    let check = |sql: &str, expected: &[&[Value]]| {
        let result = engine.sql(sql, &[]).unwrap();
        assert_eq!(result.rows.len(), expected.len(), "{provider:?}: {sql}");
        for (row, values) in expected.iter().enumerate() {
            assert_eq!(result.columns.len(), values.len());
            for (column, value) in values.iter().enumerate() {
                assert_eq!(
                    result.value_at(row, column),
                    Some(value),
                    "{provider:?}: {sql} [{row},{column}]"
                );
            }
        }
    };
    check("WITH q AS NOT MATERIALIZED (SELECT id,10/(id-1) AS unused FROM items) SELECT id FROM q ORDER BY id", &[&[Value::Int(1)],&[Value::Int(2)],&[Value::Int(3)]]);
    check("WITH q AS NOT MATERIALIZED (SELECT id,12/id AS quotient FROM items) SELECT id,quotient FROM q WHERE id>1 ORDER BY id", &[&[Value::Int(2),Value::Int(6)],&[Value::Int(3),Value::Int(4)]]);
    check("WITH q AS NOT MATERIALIZED (SELECT id FROM items), r AS NOT MATERIALIZED (SELECT id FROM q) SELECT id FROM (WITH q AS (SELECT 99 AS id) SELECT id FROM r) nested ORDER BY id", &[&[Value::Int(1)],&[Value::Int(2)],&[Value::Int(3)]]);
    check(
        "WITH q AS NOT MATERIALIZED (SELECT nextval('effect') AS n) SELECT a.n,b.n FROM q a,q b",
        &[&[Value::Int(1), Value::Int(1)]],
    );
    check("SELECT last_value FROM effect", &[&[Value::Int(1)]]);
    check("WITH RECURSIVE path(n) AS NOT MATERIALIZED (SELECT 1 UNION ALL SELECT n+1 FROM path WHERE n<3) SEARCH DEPTH FIRST BY n SET ordered CYCLE n SET cycle USING visited SELECT n,cycle FROM path ORDER BY ordered", &[&[Value::Int(1),Value::Bool(false)],&[Value::Int(2),Value::Bool(false)],&[Value::Int(3),Value::Bool(false)]]);
    engine.sql("PREPARE selected(integer) AS WITH q AS NOT MATERIALIZED (SELECT id,10/(id-1) AS unused FROM items) SELECT id FROM q WHERE id>$1 ORDER BY id; SET plan_cache_mode=force_generic_plan", &[]).unwrap();
    check("EXECUTE selected(1)", &[&[Value::Int(2)], &[Value::Int(3)]]);
    engine
        .sql("SET plan_cache_mode=force_custom_plan", &[])
        .unwrap();
    check(
        "EXECUTE selected(0)",
        &[&[Value::Int(1)], &[Value::Int(2)], &[Value::Int(3)]],
    );
    engine.sql("BEGIN; DECLARE folded_cursor CURSOR FOR WITH q AS NOT MATERIALIZED (SELECT id FROM items) SELECT id FROM q ORDER BY id", &[]).unwrap();
    check("FETCH 1 FROM folded_cursor", &[&[Value::Int(1)]]);
    check(
        "FETCH ALL FROM folded_cursor",
        &[&[Value::Int(2)], &[Value::Int(3)]],
    );
    engine
        .sql("CLOSE folded_cursor; SAVEPOINT before_effect", &[])
        .unwrap();
    check("WITH changed AS (UPDATE items SET id=id+10 RETURNING id), chosen AS NOT MATERIALIZED (SELECT id FROM changed) SELECT id FROM chosen ORDER BY id", &[&[Value::Int(11)],&[Value::Int(12)],&[Value::Int(13)]]);
    engine.sql("ROLLBACK TO before_effect; COMMIT; EXPLAIN(MEMORY,FORMAT JSON) WITH q AS NOT MATERIALIZED (SELECT nextval('effect') AS n) SELECT n FROM q; DEALLOCATE selected", &[]).unwrap();
    check("SELECT last_value FROM effect", &[&[Value::Int(1)]]);
    check(
        "SELECT id FROM items ORDER BY id",
        &[&[Value::Int(1)], &[Value::Int(2)], &[Value::Int(3)]],
    );
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        let result = reopened.sql("WITH q AS NOT MATERIALIZED (SELECT id FROM folded_reference.items) SELECT id FROM q ORDER BY id", &[]).unwrap();
        assert_eq!(result.rows.len(), 3);
        for index in 0..3 {
            assert_eq!(
                result.value_at(index, 0),
                Some(&Value::Int(index as i64 + 1))
            );
        }
        reopened.close().unwrap();
    }
}
