//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A statement keeps the output metadata produced by its ordered input analysis.

use super::*;

#[test]
fn executable_analysis_derives_each_query_result_once() {
    for sql in [
        "SELECT id FROM items",
        "SELECT id::numeric(12, 2), 'text', NULL FROM items",
        "CREATE VIEW saved AS SELECT id FROM items",
        "CREATE TABLE saved AS SELECT id FROM items",
        "CREATE MATERIALIZED VIEW saved AS SELECT id FROM items",
        "DECLARE saved CURSOR FOR SELECT id FROM items",
        "EXPLAIN SELECT id FROM items",
    ] {
        crate::binding::preparation::BLOCK_ANALYSES.with(|count| count.set(0));
        analyze(&Scopes::default(), sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
        let analyses = crate::binding::preparation::BLOCK_ANALYSES.with(std::cell::Cell::get);
        assert_eq!(analyses, 1, "{sql}");
    }
}

#[test]
fn executable_analysis_retains_postgresql_result_metadata() {
    // PostgreSQL 18.4's psql \gdesc supplies these names and declared types, including unknown targets converted to text.
    for (sql, columns, types) in [
        (
            "SELECT id::numeric(12,2) AS price, 'v' AS label, NULL AS unknown FROM items",
            vec!["price", "label", "unknown"],
            vec!["numeric(12,2)", "text", "text"],
        ),
        (
            "VALUES (1, 'v'), (2, NULL)",
            vec!["column1", "column2"],
            vec!["integer", "text"],
        ),
        (
            "WITH changed AS (UPDATE items SET id = id + 1 RETURNING id) SELECT id, 'x' AS label FROM changed",
            vec!["id", "label"],
            vec!["integer", "text"],
        ),
        (
            "UPDATE items SET id = 3 RETURNING id, 'changed' AS label",
            vec!["id", "label"],
            vec!["integer", "text"],
        ),
        (
            "SELECT id AS same, id::bigint AS same FROM items",
            vec!["same", "same"],
            vec!["integer", "bigint"],
        ),
    ] {
        let scopes = Scopes::default();
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let result = analyze_executable_plan(
            &StatementAnalysisContext {
                scopes: &scopes,
                routines: &NoRoutines,
                aliases: &NoRoutines,
            },
            &mut plan,
            &[],
        )
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
        let AnalyzedResult::Schema(schema) = result else {
            panic!("missing output schema: {sql}");
        };
        assert_eq!(schema.columns(), columns, "{sql}");
        let expected = types
            .iter()
            .map(|ty| Some(ColumnType::from_sql_name(ty).unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(schema.column_types(), expected, "{sql}");
        for (position, ty) in expected.iter().enumerate() {
            assert_eq!(
                schema.column_collation(position),
                Some(if matches!(ty, Some(ColumnType::Text)) { 100 } else { 0 }),
                "{sql}: output {position}"
            );
        }
    }
}

#[test]
fn executable_analysis_retains_parameter_identity_and_anonymous_record_fields() {
    let scopes = Scopes::default();
    let mut plan = UnifiedPlan::lower(
        crate::compile("SELECT $1 AS value, ROW(id, 'v'::text) AS r FROM items")
            .unwrap()
            .remove(0),
    );
    let result = analyze_executable_plan(
        &StatementAnalysisContext {
            scopes: &scopes,
            routines: &NoRoutines,
            aliases: &NoRoutines,
        },
        &mut plan,
        &[SQLParam::typed_scalar(Value::Int(7), ColumnType::Integer)],
    )
    .unwrap();
    assert_eq!(
        result.column_types().unwrap(),
        [Some(ColumnType::Integer), Some(ColumnType::Record)]
    );
    assert_eq!(
        result.record_fields(1).unwrap().as_ref(),
        [Some(ColumnType::Integer), Some(ColumnType::Text)]
    );
}
