//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_execution::query::sources::build_join_operator_with_ctes;
use uqa_sql::plan::{RelationalPlan, SourcePlan};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_source_binding_admits_private_function_syntax_before_copying(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("source-binding-owners.db"));
    let payload = "x".repeat(4 << 20);
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(&format!(
        "SELECT payload FROM (SELECT '{payload}'::text AS payload FROM generate_series(1,1)) AS derived"
    )).unwrap().remove(0)) else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    let source = block.from.as_ref().unwrap();
    let SourcePlan::Subquery { body, .. } = source else {
        panic!("derived source")
    };
    let outer = uqa_execution::OwnedPhysicalRow::new(
        uqa_sql::RowSchema::default(),
        uqa_execution::PhysicalRow::default(),
    );
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let runtime = engine.query_runtime_view();
    let mut scope = crate::capabilities::query_scope::new_for_statement(&engine, None).unwrap();
    let context = engine.source_execution_context();
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    for entry in ["derived", "lateral", "dml"] {
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = Some(match entry {
                "derived" => {
                    build_join_operator_with_ctes(&context, source, &[], &mut scope, None, None)
                        .map(drop)
                }
                "lateral" => {
                    uqa_execution::query::sources::lateral_query::execute_lateral_subquery_output(
                        &context,
                        body,
                        &outer,
                        &[],
                        &scope,
                    )
                    .map(drop)
                }
                "dml" => uqa_execution::query::sources::build_join_spill_with_ctes(
                    &context,
                    source,
                    &[],
                    &mut scope,
                )
                .map(drop),
                _ => unreachable!(),
            });
        });
        assert!(
            allocation.bytes_total < 256 << 10,
            "{provider:?} {entry}: {allocation:?}"
        );
        assert_eq!(
            result.unwrap().err().unwrap().sqlstate(),
            Some("53200"),
            "{entry}"
        );
        assert_eq!(memory.used(), baseline + occupied.bytes(), "{entry}");
    }
    drop(occupied);
    assert_eq!(memory.used(), baseline);
    let mut operator =
        build_join_operator_with_ctes(&context, source, &[], &mut scope, None, None).unwrap();
    assert!(
        memory.used() >= baseline + payload.len(),
        "{provider:?}: physical payload lost its allowance"
    );
    operator.open().unwrap();
    let rows = operator.next().unwrap().unwrap().into_result_rows();
    assert!(matches!(rows[0].values().next(), Some(Value::Str(value)) if value == &payload));
    operator.close().unwrap();
    drop((rows, operator));
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
fn native_source_binding_preserves_pg18_names_functions_and_effects(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source-binding-behavior.db");
    let engine = provider.open(&path);
    engine.sql("CREATE SCHEMA binding_reference; SET search_path=binding_reference; CREATE TABLE items(id integer PRIMARY KEY); INSERT INTO items VALUES(1),(2),(3); CREATE SEQUENCE effect; CREATE FUNCTION series_after(integer) RETURNS SETOF integer LANGUAGE SQL AS 'SELECT generate_series($1,$1+1)'", &[]).unwrap();
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
    check("SELECT g,u,n,pg_typeof(g)::text,pg_typeof(u)::text,pg_typeof(n)::text FROM ROWS FROM(generate_series(1,3),unnest(ARRAY['alpha','beta'])) WITH ORDINALITY AS input(g,u,n) ORDER BY n", &[
        &[Value::Int(1), Value::Str("alpha".into()), Value::Int(1), Value::Str("integer".into()), Value::Str("text".into()), Value::Str("bigint".into())],
        &[Value::Int(2), Value::Str("beta".into()), Value::Int(2), Value::Str("integer".into()), Value::Str("text".into()), Value::Str("bigint".into())],
        &[Value::Int(3), Value::Null, Value::Int(3), Value::Str("integer".into()), Value::Str("text".into()), Value::Str("bigint".into())],
    ]);
    check("SELECT o.key,s.key,s.n FROM (VALUES(9)) AS o(key) CROSS JOIN LATERAL (SELECT i.v%2 AS key,count(*) AS n FROM (VALUES(1),(2),(3)) AS i(v) GROUP BY key) AS s ORDER BY s.key", &[
        &[Value::Int(9),Value::Int(0),Value::Int(1)], &[Value::Int(9),Value::Int(1),Value::Int(2)],
    ]);
    check("SELECT o.key,s.key,s.n FROM (VALUES(9)) AS o(key) CROSS JOIN LATERAL (SELECT i.key%2 AS key,count(*) AS n FROM (VALUES(1),(2),(3)) AS i(key) GROUP BY key) AS s ORDER BY s.key,s.n", &[
        &[Value::Int(9),Value::Int(0),Value::Int(1)], &[Value::Int(9),Value::Int(1),Value::Int(1)], &[Value::Int(9),Value::Int(1),Value::Int(1)],
    ]);
    check("SELECT i.id,s.n FROM items AS i CROSS JOIN LATERAL series_after(i.id) AS s(n) WHERE i.id=2 ORDER BY s.n", &[&[Value::Int(2),Value::Int(2)], &[Value::Int(2),Value::Int(3)]]);
    check("SELECT k FROM (SELECT DISTINCT n%2 AS k FROM generate_series(1,4) AS input(n) ORDER BY k LIMIT 1 OFFSET 1) AS derived", &[&[Value::Int(1)]]);
    check("SELECT n FROM (SELECT nextval('effect') AS n FROM generate_series(1,3)) AS derived LIMIT 1", &[&[Value::Int(1)]]);
    check("SELECT last_value FROM effect", &[&[Value::Int(1)]]);
    engine.sql("PREPARE selected(integer) AS SELECT i.id,s.n FROM items AS i CROSS JOIN LATERAL series_after(i.id) AS s(n) WHERE i.id=$1 ORDER BY s.n; SET plan_cache_mode=force_generic_plan", &[]).unwrap();
    check(
        "EXECUTE selected(2)",
        &[
            &[Value::Int(2), Value::Int(2)],
            &[Value::Int(2), Value::Int(3)],
        ],
    );
    engine
        .sql("SET plan_cache_mode=force_custom_plan", &[])
        .unwrap();
    check(
        "EXECUTE selected(3)",
        &[
            &[Value::Int(3), Value::Int(3)],
            &[Value::Int(3), Value::Int(4)],
        ],
    );
    engine.sql("BEGIN; DECLARE binding_cursor CURSOR FOR SELECT i.id,s.n FROM items AS i CROSS JOIN LATERAL series_after(i.id) AS s(n) WHERE i.id=1 ORDER BY s.n", &[]).unwrap();
    check(
        "FETCH 1 FROM binding_cursor",
        &[&[Value::Int(1), Value::Int(1)]],
    );
    check(
        "FETCH ALL FROM binding_cursor",
        &[&[Value::Int(1), Value::Int(2)]],
    );
    engine
        .sql("CLOSE binding_cursor; SAVEPOINT binding_failure", &[])
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT * FROM generate_series(1,2) AS bad(a,b)", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42P10")
    );
    assert_eq!(
        engine.sql("SELECT 1", &[]).unwrap_err().sqlstate(),
        Some("25P02")
    );
    engine.sql("ROLLBACK TO binding_failure", &[]).unwrap();
    check(
        "SELECT g FROM generate_series(1,2) AS good(g) ORDER BY g",
        &[&[Value::Int(1)], &[Value::Int(2)]],
    );
    engine
        .sql(
            "SAVEPOINT before_mutation; INSERT INTO items SELECT n FROM series_after(4) AS s(n)",
            &[],
        )
        .unwrap();
    check(
        "SELECT id FROM items ORDER BY id",
        &[
            &[Value::Int(1)],
            &[Value::Int(2)],
            &[Value::Int(3)],
            &[Value::Int(4)],
            &[Value::Int(5)],
        ],
    );
    engine
        .sql("ROLLBACK TO before_mutation; SAVEPOINT before_update", &[])
        .unwrap();
    check("WITH changed AS (UPDATE items SET id=items.id+10 FROM series_after(2) AS s(n) WHERE items.id=s.n RETURNING items.id) SELECT id FROM changed ORDER BY id", &[&[Value::Int(12)], &[Value::Int(13)]]);
    engine.sql("ROLLBACK TO before_update; COMMIT; EXPLAIN(MEMORY,FORMAT JSON) SELECT n FROM (SELECT nextval('effect') AS n FROM generate_series(1,3)) AS derived LIMIT 1; DEALLOCATE selected", &[]).unwrap();
    check("SELECT last_value FROM effect", &[&[Value::Int(1)]]);
    routine_parameter_observations(&engine);
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        let result = reopened.sql("SELECT i.id,s.n FROM binding_reference.items AS i CROSS JOIN LATERAL binding_reference.series_after(i.id) AS s(n) WHERE i.id=2 ORDER BY s.n", &[]).unwrap();
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.value_at(0, 0), Some(&Value::Int(2)));
        assert_eq!(result.value_at(0, 1), Some(&Value::Int(2)));
        assert_eq!(result.value_at(1, 0), Some(&Value::Int(2)));
        assert_eq!(result.value_at(1, 1), Some(&Value::Int(3)));
        for sql in [
            "SELECT binding_reference.parameter_insert(71)",
            "SELECT binding_reference.parameter_delete(71)",
        ] {
            let result = reopened.sql(sql, &[]).unwrap();
            assert_eq!(result.value_at(0, 0), Some(&Value::Int(71)));
        }
        reopened.close().unwrap();
    }
}

fn routine_parameter_observations(engine: &crate::Engine) {
    engine.sql("CREATE TABLE binding_reference.routine_target(id integer PRIMARY KEY); CREATE PROCEDURE binding_reference.accept_pair(x integer,y integer) LANGUAGE sql AS 'SELECT x+y'; CREATE FUNCTION binding_reference.call_named(v integer) RETURNS void LANGUAGE sql AS 'CALL binding_reference.accept_pair(y => v,x => 1)'", &[]).unwrap();
    let result = engine
        .sql("SELECT binding_reference.call_named(4)", &[])
        .unwrap();
    assert_eq!(result.value_at(0, 0), Some(&Value::Null));
    for (name, body, value) in [
        ("parameter_insert", "INSERT INTO binding_reference.routine_target VALUES(n)", 61),
        ("parameter_update", "UPDATE binding_reference.routine_target SET id=n", 62),
        ("parameter_merge", "MERGE INTO binding_reference.routine_target USING (VALUES(62)) s(k) ON routine_target.id=s.k WHEN MATCHED THEN UPDATE SET id=n", 63),
        ("parameter_delete", "DELETE FROM binding_reference.routine_target WHERE id=n", 63),
    ] {
        engine.sql(&format!("CREATE FUNCTION binding_reference.{name}(n integer) RETURNS integer LANGUAGE sql AS 'WITH x AS ({body} RETURNING n) SELECT x.n FROM x'"), &[]).unwrap();
        let result = engine.sql(&format!("SELECT binding_reference.{name}({value})"), &[]).unwrap();
        assert_eq!(result.value_at(0, 0), Some(&Value::Int(value)), "{name}");
    }
}
