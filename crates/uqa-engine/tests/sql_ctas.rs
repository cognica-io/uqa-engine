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

#[test]
fn ctas_with_no_data_builds_the_typed_schema_without_executing_the_query() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE source (small SMALLINT, label VARCHAR(3), embedding VECTOR(2), features TENSOR(2))",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO source (small, label, embedding) VALUES (7, 'abc', ARRAY[1.0, 0.0])",
        &[],
    )
    .unwrap();
    eng.sql("CREATE SEQUENCE no_data_sequence START WITH 41", &[])
        .unwrap();

    let created = eng
        .sql(
            "CREATE TABLE empty_copy (renamed) AS \
             SELECT small, label, nextval('no_data_sequence') AS sequence_value, \
                    coalesce(source._doc_id, 0::bigint) AS document_id, \
                    coalesce(_doc_id, 0::bigint) AS unqualified_document_id, \
                    (SELECT coalesce(source._doc_id, 0::bigint)) AS correlated_document_id, \
                    embedding, features, 1 / 0 AS failure \
             FROM source WITH NO DATA",
            &[],
        )
        .unwrap();
    assert_eq!(created.affected_rows, 0);
    let empty = eng
        .sql(
            "SELECT renamed, label, sequence_value, document_id, unqualified_document_id, correlated_document_id, embedding, features, failure FROM empty_copy",
            &[],
        )
        .unwrap();
    assert!(empty.rows.is_empty());
    assert_eq!(
        empty.column_types,
        [
            Some(ColumnType::SmallInteger),
            Some(ColumnType::Varchar(Some(3))),
            Some(ColumnType::BigInteger),
            Some(ColumnType::BigInteger),
            Some(ColumnType::BigInteger),
            Some(ColumnType::BigInteger),
            Some(ColumnType::Vector(2)),
            Some(ColumnType::Tensor(2)),
            Some(ColumnType::Integer),
        ]
    );

    let first_sequence_value = eng.sql("SELECT nextval('no_data_sequence')", &[]).unwrap();
    assert_eq!(first_sequence_value.value_at(0, 0), Some(&Value::Int(41)));

    eng.sql(
        "INSERT INTO empty_copy (renamed, label, embedding) \
         VALUES (8, 'xy', ARRAY[0.0, 1.0])",
        &[],
    )
    .unwrap();
    let hits = eng
        .knn_search("empty_copy", "embedding", vec![0.0, 1.0], 1)
        .unwrap();
    assert_eq!(hits.first().map(|hit| hit.doc_id), Some(1));

    eng.sql(
        "CREATE TABLE populated AS \
         SELECT nextval('no_data_sequence') AS sequence_value WITH DATA",
        &[],
    )
    .unwrap();
    let populated = eng
        .sql("SELECT sequence_value FROM populated", &[])
        .unwrap();
    assert_eq!(populated.value_at(0, 0), Some(&Value::Int(42)));
}

#[test]
fn ctas_with_no_data_matches_postgresql_analysis_and_if_not_exists_order() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE source (value INTEGER)", &[]).unwrap();
    for (table, sql, sqlstate) in [
        (
            "missing_source",
            "CREATE TABLE missing_source AS \
             SELECT * FROM does_not_exist WITH NO DATA",
            "42P01",
        ),
        (
            "missing_column",
            "CREATE TABLE missing_column AS \
             SELECT absent FROM source WITH NO DATA",
            "42703",
        ),
        (
            "missing_function",
            "CREATE TABLE missing_function AS \
             SELECT does_not_exist(1) WITH NO DATA",
            "42883",
        ),
        (
            "invalid_cast",
            "CREATE TABLE invalid_cast AS \
             SELECT 'bad'::integer WITH NO DATA",
            "22P02",
        ),
        (
            "too_many",
            "CREATE TABLE too_many (a, b) AS SELECT 1 WITH NO DATA",
            "42601",
        ),
        (
            "duplicate_names",
            "CREATE TABLE duplicate_names AS \
             SELECT 1 AS value, 2 AS value WITH NO DATA",
            "42701",
        ),
    ] {
        let error = eng.sql(sql, &[]).expect_err(sql);
        assert_eq!(error.sqlstate(), Some(sqlstate), "{sql}: {error}");
        assert!(
            !eng.has_table(table).unwrap(),
            "{table} leaked after {error}"
        );
    }

    let empty_input_error = eng
        .sql(
            "CREATE TABLE missing_function_with_data AS \
             SELECT does_not_exist(value) FROM source",
            &[],
        )
        .unwrap_err();
    assert_eq!(empty_input_error.sqlstate(), Some("42883"));
    assert!(!eng.has_table("missing_function_with_data").unwrap());

    eng.sql(
        "CREATE TABLE deferred_cast AS \
         SELECT ('bad'::text)::integer AS value WITH NO DATA",
        &[],
    )
    .unwrap();
    assert!(eng
        .sql("SELECT value FROM deferred_cast", &[])
        .unwrap()
        .rows
        .is_empty());

    eng.sql("CREATE TABLE existing (kept INTEGER)", &[])
        .unwrap();
    eng.sql(
        "CREATE TABLE IF NOT EXISTS existing (a, b) AS \
         SELECT 1 / 0 WITH NO DATA",
        &[],
    )
    .unwrap();
    let missing_source = eng
        .sql(
            "CREATE TABLE IF NOT EXISTS existing AS \
             SELECT * FROM does_not_exist WITH NO DATA",
            &[],
        )
        .unwrap_err();
    assert_eq!(missing_source.sqlstate(), Some("42P01"));
}

#[test]
fn ctas_with_no_data_schema_and_vector_index_survive_reopen() {
    let directory = tempfile::TempDir::new().unwrap();
    let database = directory.path().join("ctas-no-data.db");
    {
        let eng = Engine::open(&database).unwrap();
        eng.sql(
            "CREATE TABLE source (embedding VECTOR(2), features TENSOR(2), label VARCHAR(3))",
            &[],
        )
        .unwrap();
        eng.sql(
            "CREATE TABLE durable (renamed) AS \
             SELECT embedding, features, label FROM source WITH NO DATA",
            &[],
        )
        .unwrap();
    }

    let reopened = Engine::open(&database).unwrap();
    let empty = reopened
        .sql("SELECT renamed, features, label FROM durable", &[])
        .unwrap();
    assert!(empty.rows.is_empty());
    assert_eq!(
        empty.column_types,
        [
            Some(ColumnType::Vector(2)),
            Some(ColumnType::Tensor(2)),
            Some(ColumnType::Varchar(Some(3))),
        ]
    );
    reopened
        .sql(
            "INSERT INTO durable (renamed, label) VALUES (ARRAY[1.0, 0.0], 'xy')",
            &[],
        )
        .unwrap();
    let hits = reopened
        .knn_search("durable", "renamed", vec![1.0, 0.0], 1)
        .unwrap();
    assert_eq!(hits.first().map(|hit| hit.doc_id), Some(1));
}

#[test]
fn select_into_materializes_exact_types_without_evaluating_empty_rows() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE select_into_source (small SMALLINT, label VARCHAR(3), embedding VECTOR(2), features TENSOR(2))",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO select_into_source (small, label, embedding) \
         VALUES (7, 'abc', ARRAY[1.0, 0.0])",
        &[],
    )
    .unwrap();
    eng.sql("CREATE SEQUENCE select_into_sequence START WITH 41", &[])
        .unwrap();

    let created = eng
        .sql(
            "SELECT small AS renamed, label, embedding, features, \
                    nextval('select_into_sequence') AS sequence_value \
             INTO TABLE select_into_copy \
             FROM select_into_source",
            &[],
        )
        .unwrap();
    assert_eq!(created.affected_rows, 1);
    let copied = eng
        .sql(
            "SELECT renamed, label, embedding, features, sequence_value \
             FROM select_into_copy",
            &[],
        )
        .unwrap();
    assert_eq!(
        copied.column_types,
        [
            Some(ColumnType::SmallInteger),
            Some(ColumnType::Varchar(Some(3))),
            Some(ColumnType::Vector(2)),
            Some(ColumnType::Tensor(2)),
            Some(ColumnType::BigInteger),
        ]
    );
    assert_eq!(copied.value_at(0, 0), Some(&Value::Int(7)));
    let hits = eng
        .knn_search("select_into_copy", "embedding", vec![1.0, 0.0], 1)
        .unwrap();
    assert_eq!(hits.first().map(|hit| hit.doc_id), Some(1));

    eng.sql(
        "CREATE TABLE select_into_empty_source (small SMALLINT)",
        &[],
    )
    .unwrap();
    let empty_created = eng
        .sql(
            "SELECT nextval('select_into_sequence') AS sequence_value, \
                    small / 0 AS failure \
             INTO select_into_empty \
             FROM select_into_empty_source",
            &[],
        )
        .unwrap();
    assert_eq!(empty_created.affected_rows, 0);
    let empty = eng
        .sql("SELECT sequence_value, failure FROM select_into_empty", &[])
        .unwrap();
    assert!(empty.rows.is_empty());
    assert_eq!(
        empty.column_types,
        [Some(ColumnType::BigInteger), Some(ColumnType::Integer)]
    );
    let next = eng
        .sql("SELECT nextval('select_into_sequence')", &[])
        .unwrap();
    assert_eq!(next.value_at(0, 0), Some(&Value::Int(42)));

    eng.sql(
        "PREPARE make_select_into AS \
         SELECT 9::smallint AS value INTO prepared_select_into",
        &[],
    )
    .unwrap();
    let prepared = eng.sql("EXECUTE make_select_into", &[]).unwrap();
    assert_eq!(prepared.affected_rows, 1);
    let prepared_copy = eng
        .sql("SELECT value FROM prepared_select_into", &[])
        .unwrap();
    assert_eq!(prepared_copy.column_types, [Some(ColumnType::SmallInteger)]);
    assert_eq!(prepared_copy.value_at(0, 0), Some(&Value::Int(9)));
}

#[test]
fn select_into_matches_postgresql_validation_order_and_transactionality() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE select_into_source (value INTEGER)", &[])
        .unwrap();
    for (table, sql, sqlstate) in [
        (
            "select_into_missing_source",
            "SELECT value INTO select_into_missing_source \
             FROM relation_that_does_not_exist",
            "42P01",
        ),
        (
            "select_into_missing_function",
            "SELECT function_that_does_not_exist(1) \
             INTO select_into_missing_function",
            "42883",
        ),
        (
            "select_into_invalid_cast",
            "SELECT 'bad'::integer INTO select_into_invalid_cast",
            "22P02",
        ),
        (
            "select_into_duplicate",
            "SELECT 1 AS duplicate, 2 AS duplicate \
             INTO select_into_duplicate",
            "42701",
        ),
        (
            "select_into_system_column",
            "SELECT 1 AS ctid INTO select_into_system_column",
            "42701",
        ),
        (
            "select_into_execution_failure",
            "SELECT 1 / 0 AS value INTO select_into_execution_failure",
            "22012",
        ),
    ] {
        let error = eng.sql(sql, &[]).expect_err(sql);
        assert_eq!(error.sqlstate(), Some(sqlstate), "{sql}: {error}");
        assert!(
            !eng.has_table(table).unwrap(),
            "{table} leaked after {error}"
        );
    }

    eng.sql("CREATE TABLE select_into_existing (kept INTEGER)", &[])
        .unwrap();
    let existing = eng
        .sql(
            "SELECT 1 AS duplicate, 2 AS duplicate \
             INTO select_into_existing",
            &[],
        )
        .unwrap_err();
    assert_eq!(existing.sqlstate(), Some("42P07"));

    eng.sql("BEGIN", &[]).unwrap();
    eng.sql(
        "SELECT value INTO select_into_rolled_back FROM select_into_source",
        &[],
    )
    .unwrap();
    assert!(eng.has_table("select_into_rolled_back").unwrap());
    eng.sql("ROLLBACK", &[]).unwrap();
    assert!(!eng.has_table("select_into_rolled_back").unwrap());
}

#[test]
fn select_into_schema_and_vector_index_survive_reopen() {
    let directory = tempfile::TempDir::new().unwrap();
    let database = directory.path().join("select-into.db");
    {
        let eng = Engine::open(&database).unwrap();
        eng.sql(
            "CREATE TABLE select_into_source (embedding VECTOR(2), features TENSOR(2), label VARCHAR(3))",
            &[],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO select_into_source (embedding, label) \
             VALUES (ARRAY[1.0, 0.0], 'abc')",
            &[],
        )
        .unwrap();
        eng.sql(
            "SELECT embedding AS renamed, features, label \
             INTO durable_select_into FROM select_into_source",
            &[],
        )
        .unwrap();
    }

    let reopened = Engine::open(&database).unwrap();
    let result = reopened
        .sql(
            "SELECT renamed, features, label FROM durable_select_into",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.column_types,
        [
            Some(ColumnType::Vector(2)),
            Some(ColumnType::Tensor(2)),
            Some(ColumnType::Varchar(Some(3))),
        ]
    );
    assert_eq!(result.rows.len(), 1);
    let hits = reopened
        .knn_search("durable_select_into", "renamed", vec![1.0, 0.0], 1)
        .unwrap();
    assert_eq!(hits.first().map(|hit| hit.doc_id), Some(1));
}
