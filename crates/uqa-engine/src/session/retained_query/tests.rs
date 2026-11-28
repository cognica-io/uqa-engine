//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::tests::providers::Provider;
use uqa_sql::plan::UnifiedPlan;

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(plan) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("expected query");
    };
    *plan
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn retained_query_detaches_only_referenced_tables_and_lexical_cte_inputs(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("retained-inputs.db"));
    engine.sql("CREATE TABLE selected_rows(id integer); INSERT INTO selected_rows VALUES (7); CREATE VIEW selected_view AS SELECT id FROM selected_rows; CREATE TABLE unrelated(id integer); CREATE TABLE original(id integer); CREATE TABLE through_view(id integer); CREATE VIEW physical_view AS SELECT id FROM original; CREATE FUNCTION physical_id() RETURNS integer LANGUAGE SQL AS 'SELECT max(id) FROM original'", &[]).unwrap();
    let mut scope = crate::capabilities::query_scope::new_for_statement(&engine, None).unwrap();
    for sql in [
        "WITH original AS NOT MATERIALIZED (SELECT id FROM selected_view) SELECT * FROM original",
        "WITH through_view AS NOT MATERIALIZED (SELECT id FROM original) SELECT * FROM through_view",
        "WITH original AS NOT MATERIALIZED (SELECT id FROM unrelated) SELECT * FROM original",
    ] {
        scope.insert_deferred(query(sql).ctes.remove(0).into());
    }
    let resources = engine
        .cte_execution_context()
        .production_resources()
        .unwrap();
    for (sql, expected) in [
        ("SELECT 1", None),
        ("WITH unused AS (SELECT id FROM unrelated) SELECT 1", None),
        ("SELECT id FROM selected_view", Some("selected_rows")),
        ("SELECT id FROM through_view", Some("selected_rows")),
        ("SELECT id FROM original", Some("unrelated")),
        ("SELECT id FROM physical_view", Some("original")),
        ("SELECT physical_id()", Some("original")),
        ("WITH dropped AS (DELETE FROM selected_rows RETURNING id) SELECT 1", Some("selected_rows")),
        ("WITH original AS (VALUES (1)) SELECT * FROM original", None),
        ("WITH original AS (SELECT id FROM public.original) SELECT * FROM original", Some("original")),
        ("WITH a AS (SELECT * FROM original), original AS (SELECT * FROM through_view) SELECT * FROM a", Some("unrelated")),
    ] {
        let retained = engine
            .capture_query_input(&query(sql), &[], &scope, &resources.control())
            .unwrap();
        let tables = retained
            .inputs
            .engine
            .query_table_snapshots
            .as_ref()
            .unwrap();
        assert_eq!(
            tables
                .keys()
                .map(|table| table.name.as_str())
                .collect::<Vec<_>>(),
            expected.into_iter().collect::<Vec<_>>(),
            "{sql}"
        );
    }

    let result = engine.sql("WITH RECURSIVE source AS NOT MATERIALIZED (SELECT id FROM selected_view), walk(n) AS (SELECT id FROM source UNION ALL SELECT n + 1 FROM walk WHERE n < 9) SELECT n FROM walk ORDER BY n", &[]).unwrap();
    assert_eq!(result.rows.len(), 3);
    for (index, n) in [7, 8, 9].into_iter().enumerate() {
        assert_eq!(result.value_at(index, 0), Some(&uqa_core::Value::Int(n)));
    }
}
