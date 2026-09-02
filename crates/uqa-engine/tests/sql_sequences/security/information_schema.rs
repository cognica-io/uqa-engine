//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn information_schema_engine() -> Engine {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE sequence_information_owner",
        "CREATE ROLE sequence_information_member INHERIT",
        "CREATE ROLE sequence_information_reader",
        "CREATE SCHEMA sequence_information_space",
        "GRANT USAGE, CREATE ON SCHEMA sequence_information_space TO sequence_information_owner",
        "GRANT USAGE ON SCHEMA sequence_information_space TO sequence_information_reader",
        "SET ROLE sequence_information_owner",
        "CREATE SEQUENCE sequence_information_space.small_ids AS smallint INCREMENT BY -3 MINVALUE -30 MAXVALUE 12 START WITH 9 CYCLE CACHE 4",
        "CREATE TABLE sequence_information_space.generated_rows (serial_id serial, identity_id integer GENERATED ALWAYS AS IDENTITY)",
        "RESET ROLE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    engine
}

fn information_sequence_rows(engine: &Engine) -> Vec<Vec<Value>> {
    engine
        .sql(
            "SELECT sequence_name, data_type, numeric_precision, numeric_precision_radix, numeric_scale, start_value, minimum_value, maximum_value, increment, cycle_option FROM information_schema.sequences WHERE sequence_schema = 'sequence_information_space' ORDER BY sequence_name",
            &[],
        )
        .unwrap()
        .rows
        .into_iter()
        .map(|row| {
            [
                "sequence_name",
                "data_type",
                "numeric_precision",
                "numeric_precision_radix",
                "numeric_scale",
                "start_value",
                "minimum_value",
                "maximum_value",
                "increment",
                "cycle_option",
            ]
            .into_iter()
            .map(|column| row[column].clone())
            .collect()
        })
        .collect()
}

fn sequence_type_counts(engine: &Engine, catalog: &str, name_column: &str) -> Vec<Vec<Value>> {
    engine
        .sql(
            &format!(
                "SELECT data_type, count(*) AS count FROM {catalog} WHERE {name_column} LIKE 'implicit_type_rows_%' GROUP BY data_type ORDER BY data_type"
            ),
            &[],
        )
        .unwrap()
        .rows
        .into_iter()
        .map(|row| vec![row["data_type"].clone(), row["count"].clone()])
        .collect()
}

#[test]
fn information_schema_sequences_uses_state_dependency_and_privilege_visibility() {
    let engine = information_schema_engine();
    engine
        .sql("SET ROLE sequence_information_owner", &[])
        .unwrap();
    assert_eq!(
        information_sequence_rows(&engine),
        vec![
            vec![
                Value::Str("generated_rows_serial_id_seq".into()),
                Value::Str("integer".into()),
                Value::Int(32),
                Value::Int(2),
                Value::Int(0),
                Value::Str("1".into()),
                Value::Str("1".into()),
                Value::Str(i32::MAX.to_string()),
                Value::Str("1".into()),
                Value::Str("NO".into()),
            ],
            vec![
                Value::Str("small_ids".into()),
                Value::Str("smallint".into()),
                Value::Int(16),
                Value::Int(2),
                Value::Int(0),
                Value::Str("9".into()),
                Value::Str("-30".into()),
                Value::Str("12".into()),
                Value::Str("-3".into()),
                Value::Str("YES".into()),
            ],
        ]
    );
    engine
        .sql(
            "REVOKE ALL PRIVILEGES ON SEQUENCE sequence_information_space.small_ids FROM sequence_information_owner",
            &[],
        )
        .unwrap();
    assert_eq!(information_sequence_rows(&engine).len(), 2);
    engine.sql("RESET ROLE", &[]).unwrap();

    engine
        .sql("SET ROLE sequence_information_reader", &[])
        .unwrap();
    assert!(information_sequence_rows(&engine).is_empty());
    engine.sql("RESET ROLE", &[]).unwrap();

    engine
        .sql("SET ROLE sequence_information_owner", &[])
        .unwrap();
    engine
        .sql(
            "GRANT SELECT ON SEQUENCE sequence_information_space.small_ids TO sequence_information_reader",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "GRANT USAGE ON SEQUENCE sequence_information_space.generated_rows_serial_id_seq TO sequence_information_reader",
            &[],
        )
        .unwrap();
    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql("SET ROLE sequence_information_reader", &[])
        .unwrap();
    assert_eq!(information_sequence_rows(&engine).len(), 2);
    engine.sql("RESET ROLE", &[]).unwrap();

    engine
        .sql(
            "GRANT sequence_information_owner TO sequence_information_member",
            &[],
        )
        .unwrap();
    engine
        .sql("SET ROLE sequence_information_member", &[])
        .unwrap();
    assert_eq!(information_sequence_rows(&engine).len(), 2);
}

#[test]
fn implicit_sequence_types_and_information_schema_visibility_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("implicit-sequence-types.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE TABLE implicit_type_rows (
                    small_a smallserial,
                    small_b serial2,
                    integer_a serial,
                    integer_b serial4,
                    bigint_a bigserial,
                    bigint_b serial8,
                    small_identity smallint GENERATED ALWAYS AS IDENTITY,
                    integer_identity integer GENERATED ALWAYS AS IDENTITY,
                    bigint_identity bigint GENERATED ALWAYS AS IDENTITY
                )",
                &[],
            )
            .unwrap();
        assert_implicit_sequence_types(&engine);
    }
    assert_implicit_sequence_types(&Engine::open(&database).unwrap());
}

fn assert_implicit_sequence_types(engine: &Engine) {
    assert_eq!(
        sequence_type_counts(engine, "pg_catalog.pg_sequences", "sequencename"),
        vec![
            vec![Value::Str("bigint".into()), Value::Int(3)],
            vec![Value::Str("integer".into()), Value::Int(3)],
            vec![Value::Str("smallint".into()), Value::Int(3)],
        ]
    );
    assert_eq!(
        sequence_type_counts(engine, "information_schema.sequences", "sequence_name"),
        vec![
            vec![Value::Str("bigint".into()), Value::Int(2)],
            vec![Value::Str("integer".into()), Value::Int(2)],
            vec![Value::Str("smallint".into()), Value::Int(2)],
        ]
    );
}

#[test]
fn smallserial_uses_smallint_sequence_bounds() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE smallserial_bound_rows (id smallserial)", &[])
        .unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "SELECT setval('smallserial_bound_rows_id_seq', 32768)"
        ),
        "22003"
    );
    engine
        .sql(
            "SELECT setval('smallserial_bound_rows_id_seq', 32767, false)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO smallserial_bound_rows (id) VALUES (DEFAULT)",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(&engine, "SELECT id AS v FROM smallserial_bound_rows"),
        Value::Int(32767)
    );
    assert_eq!(
        sqlstate(
            &engine,
            "INSERT INTO smallserial_bound_rows (id) VALUES (DEFAULT)"
        ),
        "2200H"
    );
}

fn catalog_sequence_names(engine: &Engine, catalog: &str, name_column: &str) -> Vec<Value> {
    engine
        .sql(
            &format!(
                "SELECT {name_column} AS name FROM {catalog} WHERE {name_column} LIKE 'session_temp_%' ORDER BY {name_column}"
            ),
            &[],
        )
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row["name"].clone())
        .collect()
}

fn assert_session_sequence_catalogs(engine: &Engine, expected: &[Value]) {
    assert_eq!(
        catalog_sequence_names(engine, "information_schema.sequences", "sequence_name"),
        expected
    );
    assert_eq!(
        catalog_sequence_names(engine, "pg_catalog.pg_sequences", "sequencename"),
        expected
    );
}

#[test]
fn sequence_catalogs_hide_other_session_temporary_namespaces() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("temporary-catalogs.sqlite")).unwrap();
    let first = root.new_session().unwrap();
    let second = root.new_session().unwrap();
    first
        .sql("CREATE TEMP SEQUENCE session_temp_first", &[])
        .unwrap();
    second
        .sql("CREATE TEMP SEQUENCE session_temp_second", &[])
        .unwrap();
    assert_session_sequence_catalogs(&first, &[Value::Str("session_temp_first".into())]);
    assert_session_sequence_catalogs(&second, &[Value::Str("session_temp_second".into())]);
    assert_session_sequence_catalogs(&root, &[]);
}
