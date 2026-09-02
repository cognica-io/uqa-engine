//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn direct_sequence_scan_tracks_the_postgresql_physical_tuple() {
    let engine = Engine::new();
    engine.sql("CREATE SEQUENCE physical_ids", &[]).unwrap();

    let initial = engine
        .sql(
            "SELECT last_value, log_cnt, is_called FROM physical_ids",
            &[],
        )
        .unwrap();
    assert_eq!(initial.rows[0]["last_value"], Value::Int(1));
    assert_eq!(initial.rows[0]["log_cnt"], Value::Int(0));
    assert_eq!(initial.rows[0]["is_called"], Value::Bool(false));

    assert_eq!(
        engine
            .sql("SELECT nextval('physical_ids') AS v", &[])
            .unwrap()
            .rows[0]["v"],
        Value::Int(1)
    );
    let first = engine.sql("SELECT * FROM ONLY physical_ids", &[]).unwrap();
    assert_eq!(first.rows[0]["last_value"], Value::Int(1));
    assert_eq!(first.rows[0]["log_cnt"], Value::Int(32));
    assert_eq!(first.rows[0]["is_called"], Value::Bool(true));

    engine.sql("SELECT nextval('physical_ids')", &[]).unwrap();
    let second = engine.sql("SELECT * FROM physical_ids", &[]).unwrap();
    assert_eq!(second.rows[0]["last_value"], Value::Int(2));
    assert_eq!(second.rows[0]["log_cnt"], Value::Int(31));

    engine
        .sql("SELECT setval('physical_ids', 7, false)", &[])
        .unwrap();
    let reset = engine.sql("SELECT * FROM physical_ids", &[]).unwrap();
    assert_eq!(reset.rows[0]["last_value"], Value::Int(7));
    assert_eq!(reset.rows[0]["log_cnt"], Value::Int(0));
    assert_eq!(reset.rows[0]["is_called"], Value::Bool(false));

    engine
        .sql("ALTER SEQUENCE physical_ids RESTART WITH 5", &[])
        .unwrap();
    let restarted = engine.sql("SELECT * FROM physical_ids", &[]).unwrap();
    assert_eq!(restarted.rows[0]["last_value"], Value::Int(5));
    assert_eq!(restarted.rows[0]["log_cnt"], Value::Int(0));
    assert_eq!(restarted.rows[0]["is_called"], Value::Bool(false));
    assert_eq!(
        engine
            .sql(
                "SELECT p.last_value FROM physical_ids AS p WHERE NOT p.is_called",
                &[],
            )
            .unwrap()
            .rows[0]["last_value"],
        Value::Int(5)
    );
    assert_eq!(
        engine
            .sql("SELECT * FROM physical_ids FOR UPDATE", &[])
            .unwrap_err()
            .sqlstate(),
        Some("0A000")
    );
}

#[test]
fn cached_and_bounded_sequence_tuples_match_postgresql_reservations() {
    let engine = Engine::new();
    engine
        .sql("CREATE SEQUENCE cached_ids CACHE 10", &[])
        .unwrap();
    engine.sql("SELECT nextval('cached_ids')", &[]).unwrap();
    let reserved = engine.sql("SELECT * FROM cached_ids", &[]).unwrap();
    assert_eq!(reserved.rows[0]["last_value"], Value::Int(10));
    assert_eq!(reserved.rows[0]["log_cnt"], Value::Int(32));
    for expected in 2..=10 {
        assert_eq!(
            engine
                .sql("SELECT nextval('cached_ids') AS v", &[])
                .unwrap()
                .rows[0]["v"],
            Value::Int(expected)
        );
    }
    engine.sql("SELECT nextval('cached_ids')", &[]).unwrap();
    let next_block = engine.sql("SELECT * FROM cached_ids", &[]).unwrap();
    assert_eq!(next_block.rows[0]["last_value"], Value::Int(20));
    assert_eq!(next_block.rows[0]["log_cnt"], Value::Int(22));

    engine
        .sql(
            "CREATE SEQUENCE bounded_ids AS smallint INCREMENT 2 MINVALUE 3 MAXVALUE 9 START 5 CACHE 3 CYCLE",
            &[],
        )
        .unwrap();
    engine.sql("SELECT nextval('bounded_ids')", &[]).unwrap();
    let bounded = engine.sql("SELECT * FROM bounded_ids", &[]).unwrap();
    assert_eq!(bounded.rows[0]["last_value"], Value::Int(9));
    assert_eq!(bounded.rows[0]["log_cnt"], Value::Int(0));
    assert_eq!(bounded.rows[0]["is_called"], Value::Bool(true));
}

#[test]
fn sequence_introspection_functions_return_postgresql_records() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SEQUENCE inspect_ids AS smallint INCREMENT 2 MINVALUE 3 MAXVALUE 9 START 5 CACHE 3 CYCLE",
            &[],
        )
        .unwrap();

    let parameters = engine
        .sql(
            "SELECT * FROM pg_sequence_parameters('inspect_ids'::regclass)",
            &[],
        )
        .unwrap();
    assert_eq!(parameters.rows[0]["start_value"], Value::Int(5));
    assert_eq!(parameters.rows[0]["minimum_value"], Value::Int(3));
    assert_eq!(parameters.rows[0]["maximum_value"], Value::Int(9));
    assert_eq!(parameters.rows[0]["increment"], Value::Int(2));
    assert_eq!(parameters.rows[0]["cycle_option"], Value::Bool(true));
    assert_eq!(parameters.rows[0]["cache_size"], Value::Int(3));
    assert_eq!(parameters.rows[0]["data_type"], Value::Int(21));

    let scalar = engine
        .sql(
            "SELECT pg_sequence_parameters('inspect_ids'::regclass) AS value",
            &[],
        )
        .unwrap();
    assert!(matches!(scalar.rows[0]["value"], Value::Record(_)));

    let data = engine
        .sql("SELECT * FROM pg_get_sequence_data('inspect_ids')", &[])
        .unwrap();
    assert_eq!(data.rows[0]["last_value"], Value::Int(5));
    assert_eq!(data.rows[0]["is_called"], Value::Bool(false));
    assert_eq!(
        engine
            .sql("SELECT pg_sequence_last_value('inspect_ids') AS value", &[],)
            .unwrap()
            .rows[0]["value"],
        Value::Null
    );

    engine.sql("SELECT nextval('inspect_ids')", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT pg_sequence_last_value('inspect_ids') AS value", &[],)
            .unwrap()
            .rows[0]["value"],
        Value::Int(9)
    );

    engine
        .sql("CREATE TABLE not_a_sequence (id int)", &[])
        .unwrap();
    let wrong_kind = engine
        .sql("SELECT * FROM pg_get_sequence_data('not_a_sequence')", &[])
        .unwrap();
    assert_eq!(wrong_kind.rows[0]["last_value"], Value::Null);
    assert_eq!(wrong_kind.rows[0]["is_called"], Value::Null);
    assert_eq!(
        engine
            .sql(
                "SELECT * FROM pg_sequence_parameters('not_a_sequence'::regclass)",
                &[],
            )
            .unwrap_err()
            .sqlstate(),
        Some("XX000")
    );
    assert_eq!(
        engine
            .sql("SELECT pg_sequence_last_value('not_a_sequence')", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42809")
    );

    let strict = engine
        .sql("SELECT * FROM pg_get_sequence_data(NULL::regclass)", &[])
        .unwrap();
    assert_eq!(strict.rows[0]["last_value"], Value::Null);
    assert_eq!(strict.rows[0]["is_called"], Value::Null);
}

#[test]
fn sequence_introspection_obeys_each_postgresql_privilege_rule() {
    let engine = Engine::new();
    engine.sql("CREATE ROLE sequence_reader", &[]).unwrap();
    engine.sql("CREATE SEQUENCE secured_ids", &[]).unwrap();
    engine.sql("SELECT nextval('secured_ids')", &[]).unwrap();
    engine.sql("SET ROLE sequence_reader", &[]).unwrap();
    assert_no_sequence_introspection_privileges(&engine);

    engine.sql("RESET ROLE", &[]).unwrap();
    engine.sql("CREATE ROLE sequence_updater", &[]).unwrap();
    engine
        .sql(
            "GRANT UPDATE ON SEQUENCE secured_ids TO sequence_updater",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE sequence_updater", &[]).unwrap();
    assert_update_sequence_introspection_privileges(&engine);

    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql(
            "GRANT USAGE ON SEQUENCE secured_ids TO sequence_reader",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE sequence_reader", &[]).unwrap();
    assert_usage_sequence_introspection_privileges(&engine);

    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql(
            "GRANT SELECT ON SEQUENCE secured_ids TO sequence_reader",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE sequence_reader", &[]).unwrap();
    assert_select_sequence_introspection_privileges(&engine);
}

fn assert_no_sequence_introspection_privileges(engine: &Engine) {
    assert_eq!(
        engine
            .sql("SELECT * FROM secured_ids", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    assert_eq!(
        engine
            .sql(
                "SELECT * FROM pg_sequence_parameters('secured_ids'::regclass)",
                &[],
            )
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    assert_eq!(
        engine
            .sql("SELECT * FROM pg_get_sequence_data('secured_ids')", &[])
            .unwrap()
            .rows[0]["last_value"],
        Value::Null
    );
    assert_eq!(sequence_catalog_last_value(engine), Value::Null);
    assert_eq!(sequence_function_last_value(engine), Value::Null);
}

fn assert_update_sequence_introspection_privileges(engine: &Engine) {
    engine
        .sql(
            "SELECT * FROM pg_sequence_parameters('secured_ids'::regclass)",
            &[],
        )
        .unwrap();
    assert_eq!(sequence_function_last_value(engine), Value::Null);
    assert_eq!(
        engine
            .sql("SELECT * FROM pg_get_sequence_data('secured_ids')", &[])
            .unwrap()
            .rows[0]["last_value"],
        Value::Null
    );
    assert_eq!(sequence_catalog_last_value(engine), Value::Null);
    assert_eq!(
        engine
            .sql("SELECT * FROM secured_ids", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
}

fn assert_usage_sequence_introspection_privileges(engine: &Engine) {
    engine
        .sql(
            "SELECT * FROM pg_sequence_parameters('secured_ids'::regclass)",
            &[],
        )
        .unwrap();
    assert_eq!(sequence_function_last_value(engine), Value::Int(1));
    assert_eq!(sequence_catalog_last_value(engine), Value::Int(1));
    assert_eq!(
        engine
            .sql("SELECT * FROM secured_ids", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    assert_eq!(
        engine
            .sql("SELECT * FROM pg_get_sequence_data('secured_ids')", &[])
            .unwrap()
            .rows[0]["last_value"],
        Value::Null
    );
}

fn assert_select_sequence_introspection_privileges(engine: &Engine) {
    engine
        .sql(
            "SELECT * FROM pg_sequence_parameters('secured_ids'::regclass)",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine.sql("SELECT * FROM secured_ids", &[]).unwrap().rows[0]["last_value"],
        Value::Int(1)
    );
    assert_eq!(
        engine
            .sql("SELECT * FROM pg_get_sequence_data('secured_ids')", &[])
            .unwrap()
            .rows[0]["last_value"],
        Value::Int(1)
    );
    assert_eq!(sequence_function_last_value(engine), Value::Int(1));
    assert_eq!(sequence_catalog_last_value(engine), Value::Int(1));
}

fn sequence_function_last_value(engine: &Engine) -> Value {
    engine
        .sql("SELECT pg_sequence_last_value('secured_ids') AS value", &[])
        .unwrap()
        .rows[0]["value"]
        .clone()
}

fn sequence_catalog_last_value(engine: &Engine) -> Value {
    engine
        .sql(
            "SELECT last_value FROM pg_sequences WHERE sequencename = 'secured_ids'",
            &[],
        )
        .unwrap()
        .rows[0]["last_value"]
        .clone()
}

#[test]
fn sequence_introspection_builtin_catalog_rows_match_postgresql_18() {
    let engine = Engine::new();
    engine.sql("CREATE SEQUENCE catalog_ids", &[]).unwrap();
    let relation = engine
        .sql(
            "SELECT relkind, relnatts, reltype FROM pg_class WHERE oid = 'catalog_ids'::regclass",
            &[],
        )
        .unwrap();
    assert_eq!(relation.rows[0]["relkind"], Value::Str("S".into()));
    assert_eq!(relation.rows[0]["relnatts"], Value::Int(3));
    assert_eq!(relation.rows[0]["reltype"], Value::Int(0));
    let attributes = engine
        .sql(
            "SELECT attname, atttypid, attnum, attnotnull FROM pg_attribute WHERE attrelid = 'catalog_ids'::regclass AND attnum > 0 ORDER BY attnum",
            &[],
        )
        .unwrap();
    assert_eq!(attributes.rows.len(), 3);
    assert_eq!(
        attributes.rows[0]["attname"],
        Value::Str("last_value".into())
    );
    assert_eq!(attributes.rows[0]["atttypid"], Value::Int(20));
    assert_eq!(attributes.rows[0]["attnum"], Value::Int(1));
    assert_eq!(attributes.rows[0]["attnotnull"], Value::Bool(true));
    assert_eq!(attributes.rows[1]["attname"], Value::Str("log_cnt".into()));
    assert_eq!(attributes.rows[1]["atttypid"], Value::Int(20));
    assert_eq!(
        attributes.rows[2]["attname"],
        Value::Str("is_called".into())
    );
    assert_eq!(attributes.rows[2]["atttypid"], Value::Int(16));

    let result = engine
        .sql(
            "SELECT oid, proname, prorettype, proargtypes, proallargtypes, proargmodes, proargnames, proisstrict, provolatile, proparallel FROM pg_proc WHERE oid IN (3078, 4032, 6427) ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 3);
    assert_eq!(
        result.rows[0]["proname"],
        Value::Str("pg_sequence_parameters".into())
    );
    assert_eq!(result.rows[0]["prorettype"], Value::Int(2249));
    assert_eq!(
        result.rows[0]["proargtypes"],
        Value::List(vec![Value::Int(26)])
    );
    assert_eq!(result.rows[0]["proisstrict"], Value::Bool(true));
    assert_eq!(result.rows[0]["provolatile"], Value::Str("s".into()));
    assert_eq!(result.rows[0]["proparallel"], Value::Str("s".into()));
    let Value::Array(parameter_types) = &result.rows[0]["proallargtypes"] else {
        panic!("pg_sequence_parameters proallargtypes must be oid[]");
    };
    assert_eq!(
        parameter_types.elements(),
        &[
            Value::Int(26),
            Value::Int(20),
            Value::Int(20),
            Value::Int(20),
            Value::Int(20),
            Value::Int(16),
            Value::Int(20),
            Value::Int(26),
        ]
    );

    assert_eq!(
        result.rows[1]["proname"],
        Value::Str("pg_sequence_last_value".into())
    );
    assert_eq!(result.rows[1]["prorettype"], Value::Int(20));
    assert_eq!(result.rows[1]["provolatile"], Value::Str("v".into()));
    assert_eq!(result.rows[1]["proparallel"], Value::Str("u".into()));

    assert_eq!(
        result.rows[2]["proname"],
        Value::Str("pg_get_sequence_data".into())
    );
    assert_eq!(result.rows[2]["prorettype"], Value::Int(2249));
    assert_eq!(
        result.rows[2]["proargtypes"],
        Value::List(vec![Value::Int(2205)])
    );
    let Value::Array(argument_modes) = &result.rows[2]["proargmodes"] else {
        panic!("pg_get_sequence_data proargmodes must be char[]");
    };
    assert_eq!(
        argument_modes.elements(),
        &[
            Value::Str("i".into()),
            Value::Str("o".into()),
            Value::Str("o".into())
        ]
    );
    assert!(matches!(result.rows[2]["proargnames"], Value::Array(_)));
}

#[test]
fn durable_sequence_log_count_survives_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sequence-introspection.db");
    {
        let engine = Engine::open(&path).unwrap();
        engine
            .sql("CREATE SEQUENCE durable_cached_ids CACHE 10", &[])
            .unwrap();
        engine
            .sql("SELECT nextval('durable_cached_ids')", &[])
            .unwrap();
    }

    let reopened = Engine::open(&path).unwrap();
    let row = reopened
        .sql("SELECT * FROM durable_cached_ids", &[])
        .unwrap();
    assert_eq!(row.rows[0]["last_value"], Value::Int(10));
    assert_eq!(row.rows[0]["log_cnt"], Value::Int(32));
    assert_eq!(row.rows[0]["is_called"], Value::Bool(true));
}

#[test]
fn sequence_scans_respect_cross_kind_search_path_shadowing() {
    let engine = Engine::new();
    engine
        .sql("CREATE SEQUENCE shadowed_relation START 5", &[])
        .unwrap();
    engine
        .sql("CREATE TEMP TABLE shadowed_relation (id int)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO shadowed_relation VALUES (42)", &[])
        .unwrap();
    let table = engine.sql("SELECT * FROM shadowed_relation", &[]).unwrap();
    assert_eq!(table.rows[0]["id"], Value::Int(42));
    assert!(!table.rows[0].contains_key("last_value"));
    engine.sql("DROP TABLE shadowed_relation", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT * FROM shadowed_relation", &[])
            .unwrap()
            .rows[0]["last_value"],
        Value::Int(5)
    );

    engine
        .sql("CREATE TABLE sequence_shadow (id int)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO sequence_shadow VALUES (1)", &[])
        .unwrap();
    engine
        .sql("CREATE TEMP SEQUENCE sequence_shadow START 10", &[])
        .unwrap();
    let sequence = engine.sql("SELECT * FROM sequence_shadow", &[]).unwrap();
    assert_eq!(
        sequence.rows[0].get("last_value"),
        Some(&Value::Int(10)),
        "{:?}",
        sequence.rows
    );
    assert!(!sequence.rows[0].contains_key("id"));
    assert_eq!(
        engine
            .sql(
                "SELECT last_value FROM sequence_shadow WHERE last_value = 10",
                &[],
            )
            .unwrap()
            .rows[0]["last_value"],
        Value::Int(10)
    );
    assert_eq!(
        engine
            .sql("SELECT id FROM public.sequence_shadow", &[])
            .unwrap()
            .rows[0]["id"],
        Value::Int(1)
    );
}
