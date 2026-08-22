//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `CREATE TABLE AS SELECT` (CTAS) round-trip.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::ColumnType;

#[test]
fn ctas_materializes_select_into_new_table() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE src (id INTEGER PRIMARY KEY, name TEXT, score INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO src (id, name, score) VALUES \
         (1, 'a', 10), (2, 'b', 20), (3, 'c', 30)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE high AS SELECT id, name FROM src WHERE score >= 20",
        &[],
    )
    .unwrap();
    let r = eng.sql("SELECT id, name FROM high", &[]).unwrap();
    assert_eq!(r.rows.len(), 2);
    let names: Vec<&Value> = r.rows.iter().filter_map(|row| row.get("name")).collect();
    assert!(names.contains(&&Value::Str("b".into())));
    assert!(names.contains(&&Value::Str("c".into())));
}

#[test]
fn ctas_with_if_not_exists_skips_when_present() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE src (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("INSERT INTO src (id) VALUES (1), (2)", &[])
        .unwrap();
    eng.sql("CREATE TABLE dst AS SELECT id FROM src", &[])
        .unwrap();
    eng.sql("CREATE TABLE IF NOT EXISTS dst AS SELECT id FROM src", &[])
        .unwrap();
    let r = eng.sql("SELECT id FROM dst", &[]).unwrap();
    assert_eq!(r.rows.len(), 2);
}

#[test]
fn ctas_column_names_are_positional_partial_and_case_preserving() {
    let eng = Engine::new();
    let created = eng
        .sql(
            "CREATE TABLE exact (\"Mixed\", second) AS \
             SELECT 1::smallint AS duplicate, 'xy'::varchar(3) AS duplicate",
            &[],
        )
        .unwrap();
    assert_eq!(created.affected_rows, 1);

    let exact = eng.sql("SELECT \"Mixed\", second FROM exact", &[]).unwrap();
    assert_eq!(exact.columns, ["Mixed", "second"]);
    assert_eq!(
        exact.column_types,
        [
            Some(ColumnType::SmallInteger),
            Some(ColumnType::Varchar(Some(3))),
        ]
    );
    assert_eq!(exact.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(exact.value_at(0, 1), Some(&Value::Str("xy".into())));
    eng.sql(
        "INSERT INTO exact (\"Mixed\", second) VALUES (NULL, NULL)",
        &[],
    )
    .unwrap();
    let nullable = eng
        .sql(
            "SELECT \"Mixed\", second FROM exact WHERE \"Mixed\" IS NULL",
            &[],
        )
        .unwrap();
    assert_eq!(nullable.value_at(0, 0), Some(&Value::Null));
    assert_eq!(nullable.value_at(0, 1), Some(&Value::Null));

    eng.sql(
        "CREATE TABLE partial_alias (renamed_first) AS \
         SELECT 1 AS first, 2 AS second",
        &[],
    )
    .unwrap();
    let partial = eng
        .sql("SELECT renamed_first, second FROM partial_alias", &[])
        .unwrap();
    assert_eq!(partial.columns, ["renamed_first", "second"]);
    assert_eq!(partial.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(partial.value_at(0, 1), Some(&Value::Int(2)));

    eng.sql(
        "CREATE TABLE quoted_system_names (\"CTID\", oid) AS SELECT 3, 4",
        &[],
    )
    .unwrap();
    let quoted_system = eng
        .sql("SELECT \"CTID\", oid FROM quoted_system_names", &[])
        .unwrap();
    assert_eq!(quoted_system.value_at(0, 0), Some(&Value::Int(3)));
    assert_eq!(quoted_system.value_at(0, 1), Some(&Value::Int(4)));

    let empty_created = eng
        .sql(
            "CREATE TABLE empty_copy (renamed) AS SELECT 1::smallint WHERE false",
            &[],
        )
        .unwrap();
    assert_eq!(empty_created.affected_rows, 0);
    let empty = eng.sql("SELECT renamed FROM empty_copy", &[]).unwrap();
    assert!(empty.rows.is_empty());
    assert_eq!(empty.column_types, [Some(ColumnType::SmallInteger)]);
}

#[test]
fn ctas_column_names_match_postgresql_validation_order_and_sqlstates() {
    let eng = Engine::new();
    for (table, sql, sqlstate, message) in [
        (
            "too_many",
            "CREATE TABLE too_many (a, b, c) AS SELECT 1, 2",
            "42601",
            "too many column names",
        ),
        (
            "duplicate_alias",
            "CREATE TABLE duplicate_alias (a, a) AS SELECT 1, 2",
            "42701",
            "specified more than once",
        ),
        (
            "derived_duplicate",
            "CREATE TABLE derived_duplicate (b) AS SELECT 1 AS a, 2 AS b",
            "42701",
            "specified more than once",
        ),
        (
            "duplicate_output",
            "CREATE TABLE duplicate_output AS SELECT 1 AS a, 2 AS a",
            "42701",
            "specified more than once",
        ),
        (
            "system_alias",
            "CREATE TABLE system_alias (ctid) AS SELECT 1",
            "42701",
            "conflicts with a system column name",
        ),
        (
            "derived_system",
            "CREATE TABLE derived_system AS SELECT 1 AS tableoid",
            "42701",
            "conflicts with a system column name",
        ),
    ] {
        let error = eng.sql(sql, &[]).expect_err(sql);
        assert_eq!(error.sqlstate(), Some(sqlstate), "{sql}: {error}");
        assert!(error.to_string().contains(message), "{sql}: {error}");
        assert!(
            !eng.has_table(table).unwrap(),
            "{table} leaked after {error}"
        );
    }

    eng.sql("CREATE SEQUENCE alias_validation START WITH 41", &[])
        .unwrap();
    let alias_error = eng
        .sql(
            "CREATE TABLE alias_side_effect (a, b) AS \
             SELECT nextval('alias_validation')",
            &[],
        )
        .unwrap_err();
    assert_eq!(alias_error.sqlstate(), Some("42601"));
    let next = eng.sql("SELECT nextval('alias_validation')", &[]).unwrap();
    assert_eq!(next.value_at(0, 0), Some(&Value::Int(41)));

    let execution_error = eng
        .sql(
            "CREATE TABLE execution_failure (value) AS SELECT 1 / 0",
            &[],
        )
        .unwrap_err();
    assert_eq!(execution_error.sqlstate(), Some("22012"));
    assert!(!eng.has_table("execution_failure").unwrap());

    eng.sql("CREATE TABLE existing (kept INTEGER)", &[])
        .unwrap();
    eng.sql(
        "CREATE TABLE IF NOT EXISTS existing (a, b) AS SELECT 1 / 0",
        &[],
    )
    .unwrap();
    let existing = eng.sql("SELECT kept FROM existing", &[]).unwrap();
    assert!(existing.rows.is_empty());
    assert_eq!(existing.column_types, [Some(ColumnType::Integer)]);
}

#[test]
fn ctas_column_names_and_types_survive_reopen() {
    let directory = tempfile::TempDir::new().unwrap();
    let database = directory.path().join("ctas.db");
    {
        let eng = Engine::open(&database).unwrap();
        eng.sql(
            "CREATE TABLE durable (renamed, flag) AS \
             SELECT 7::smallint AS source, true AS source_flag",
            &[],
        )
        .unwrap();
    }

    let reopened = Engine::open(&database).unwrap();
    let result = reopened
        .sql("SELECT renamed, flag FROM durable", &[])
        .unwrap();
    assert_eq!(
        result.column_types,
        [Some(ColumnType::SmallInteger), Some(ColumnType::Boolean),]
    );
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(7)));
    assert_eq!(result.value_at(0, 1), Some(&Value::Bool(true)));
}

#[test]
fn ctas_column_names_preserve_vector_field_schema() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE source (embedding VECTOR(2))", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO source (embedding) VALUES (ARRAY[1.0, 0.0])",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE copied (renamed) AS SELECT embedding FROM source",
        &[],
    )
    .unwrap();

    let result = eng.sql("SELECT renamed FROM copied", &[]).unwrap();
    assert_eq!(result.column_types, [Some(ColumnType::Vector(2))]);
    let hits = eng
        .knn_search("copied", "renamed", vec![1.0, 0.0], 1)
        .unwrap();
    assert_eq!(hits.first().map(|hit| hit.doc_id), Some(1));
}
