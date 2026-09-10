//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 built-in ranges, temporal keys, PERIOD foreign keys, and ALTER persistence.

use tempfile::tempdir;
use uqa_core::Value;
use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) {
    engine.sql(sql, &[]).unwrap();
}

fn assert_sqlstate(engine: &Engine, sql: &str, sqlstate: &str) {
    let error = engine.sql(sql, &[]).unwrap_err();
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains(sqlstate),
        "expected SQLSTATE {sqlstate}, got {rendered}"
    );
}

#[test]
fn built_in_ranges_preserve_declared_identity_and_canonicalize_input() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE range_values (
            id integer PRIMARY KEY,
            i int4range,
            bi int8range,
            n numrange,
            d daterange,
            ts tsrange,
            tz tstzrange,
            im int4multirange,
            bim int8multirange,
            nm nummultirange,
            dm datemultirange,
            tsm tsmultirange,
            tzm tstzmultirange
        )",
    );
    exec(
        &engine,
        "INSERT INTO range_values (id, i, bi, n, d, ts, tz, im, bim, nm, dm, tsm, tzm) VALUES (
            1, '[1,4]', '(1,4]', '[1.00,2.50]', '[2024-01-01,2024-01-02]',
            '[2024-01-01 00:00:00,2024-01-02 00:00:00)',
            '[2024-01-01 00:00:00+00,2024-01-02 00:00:00+00)',
            '{[1,3),[3,5),[10,12)}', '{[1,3)}', '{[1,2)}',
            '{[2024-01-01,2024-01-02]}',
            '{[2024-01-01 00:00:00,2024-01-02 00:00:00)}',
            '{[2024-01-01 00:00:00+00,2024-01-02 00:00:00+00)}'
        )",
    );
    let result = engine
        .sql("SELECT i, bi, d, im FROM range_values WHERE id = 1", &[])
        .unwrap();
    assert_eq!(result.rows[0]["i"], Value::Str("[1,5)".into()));
    assert_eq!(result.rows[0]["bi"], Value::Str("[2,5)".into()));
    assert_eq!(
        result.rows[0]["d"],
        Value::Str("[2024-01-01,2024-01-03)".into())
    );
    assert_eq!(result.rows[0]["im"], Value::Str("{[1,5),[10,12)}".into()));
    let types = engine
        .sql(
            "SELECT typname FROM pg_type WHERE typname IN ('int4range', 'int8range', 'numrange', 'daterange', 'tsrange', 'tstzrange', 'int4multirange', 'int8multirange', 'nummultirange', 'datemultirange', 'tsmultirange', 'tstzmultirange') ORDER BY typname",
            &[],
        )
        .unwrap();
    assert_eq!(types.rows.len(), 12);
    assert_eq!(
        engine
            .sql("SELECT * FROM pg_range", &[])
            .unwrap()
            .rows
            .len(),
        6
    );
    let routines = engine
        .sql(
            "SELECT oid, proname, provariadic FROM pg_proc WHERE proname IN ('int4range', 'int4multirange', 'range_merge', 'isempty', 'lower_inc')",
            &[],
        )
        .unwrap();
    assert_eq!(routines.rows.len(), 11);
    assert!(routines
        .rows
        .iter()
        .any(|row| { row["oid"] == Value::Int(4282) && row["provariadic"] == Value::Int(3904) }));
}

#[test]
fn range_functions_operators_and_stored_generation_use_declared_subtype() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE range_functions (
            id integer PRIMARY KEY,
            valid_at int4range,
            lower_value integer GENERATED ALWAYS AS (lower(valid_at)) STORED,
            span int4range GENERATED ALWAYS AS (range_merge(valid_at, int4range(10, 12))) STORED
        )",
    );
    exec(
        &engine,
        "INSERT INTO range_functions (id, valid_at) VALUES (1, int4range(1, 4, '[]'))",
    );
    let row = engine
        .sql(
            "SELECT lower(valid_at) AS lo, upper(valid_at) AS hi,
                    lower_inc(valid_at) AS li, upper_inc(valid_at) AS ui,
                    isempty(valid_at) AS empty,
                    valid_at && '[4,8)'::int4range AS overlap,
                    valid_at @> '[2,3)'::int4range AS contains,
                    valid_at -|- '[5,8)'::int4range AS adjacent,
                    lower_value, span,
                    multirange(valid_at) AS generic_multi,
                    int4multirange(valid_at, '[8,10)'::int4range) AS named_multi
             FROM range_functions WHERE id = 1",
            &[],
        )
        .unwrap();
    let row = &row.rows[0];
    assert_eq!(row["lo"], Value::Int(1));
    assert_eq!(row["hi"], Value::Int(5));
    assert_eq!(row["li"], Value::Bool(true));
    assert_eq!(row["ui"], Value::Bool(false));
    assert_eq!(row["empty"], Value::Bool(false));
    assert_eq!(row["overlap"], Value::Bool(true));
    assert_eq!(row["contains"], Value::Bool(true));
    assert_eq!(row["adjacent"], Value::Bool(true));
    assert_eq!(row["lower_value"], Value::Int(1));
    assert_eq!(row["span"], Value::Str("[1,12)".into()));
    assert_eq!(row["generic_multi"], Value::Str("{[1,5)}".into()));
    assert_eq!(row["named_multi"], Value::Str("{[1,5),[8,10)}".into()));
}

#[test]
fn range_polymorphic_routines_link_subtypes_and_paired_multiranges() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE FUNCTION range_lower(value anyrange) RETURNS anyelement LANGUAGE SQL IMMUTABLE AS 'SELECT lower($1)'",
    );
    exec(
        &engine,
        "CREATE FUNCTION range_to_multi(value anyrange) RETURNS anymultirange LANGUAGE SQL IMMUTABLE AS 'SELECT multirange($1)'",
    );
    exec(
        &engine,
        "CREATE FUNCTION multi_to_range(value anymultirange) RETURNS anyrange LANGUAGE SQL IMMUTABLE AS 'SELECT range_merge($1)'",
    );
    exec(
        &engine,
        "CREATE FUNCTION range_scalar_pair(value anyrange, item anyelement) RETURNS anyelement LANGUAGE SQL IMMUTABLE AS 'SELECT $2'",
    );
    exec(
        &engine,
        "CREATE FUNCTION compatible_range_pair(value anycompatiblerange, item anycompatible) RETURNS anycompatible LANGUAGE SQL IMMUTABLE AS 'SELECT $2'",
    );
    let row = &engine
        .sql(
            "SELECT range_lower('[1,5)'::int4range) AS lo,
                    pg_typeof(range_lower('[1,5)'::int4range))::text AS lo_type,
                    range_to_multi('[1,5)'::int4range)::text AS multi,
                    pg_typeof(range_to_multi('[1,5)'::int4range))::text AS multi_type,
                    multi_to_range('{[1,5)}'::int4multirange)::text AS merged,
                    pg_typeof(multi_to_range('{[1,5)}'::int4multirange))::text AS merged_type,
                    range_scalar_pair('[1,5)'::int4range, 2::integer) AS paired,
                    pg_typeof(compatible_range_pair('[1,5)'::int8range, 2::integer))::text AS compatible_type",
            &[],
        )
        .unwrap()
        .rows[0];
    assert_eq!(row["lo"], Value::Int(1));
    assert_eq!(row["lo_type"], Value::Str("integer".into()));
    assert_eq!(row["multi"], Value::Str("{[1,5)}".into()));
    assert_eq!(row["multi_type"], Value::Str("int4multirange".into()));
    assert_eq!(row["merged"], Value::Str("[1,5)".into()));
    assert_eq!(row["merged_type"], Value::Str("int4range".into()));
    assert_eq!(row["paired"], Value::Int(2));
    assert_eq!(row["compatible_type"], Value::Str("bigint".into()));
    assert_sqlstate(
        &engine,
        "SELECT range_scalar_pair('[1,5)'::int4range, 2::bigint)",
        "42883",
    );
    assert_sqlstate(
        &engine,
        "SELECT compatible_range_pair('[1,5)'::int4range, 2::bigint)",
        "42883",
    );
    assert_sqlstate(&engine, "SELECT range_lower(NULL)", "42804");
}

#[test]
fn without_overlaps_and_period_foreign_key_enforce_aggregate_coverage() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE temporal_parent (
            tenant integer,
            valid_at daterange,
            CONSTRAINT temporal_parent_pk PRIMARY KEY (tenant, valid_at WITHOUT OVERLAPS)
        )",
    );
    exec(
        &engine,
        "CREATE TABLE temporal_child (
            row_id integer PRIMARY KEY,
            tenant integer,
            valid_at daterange,
            CONSTRAINT temporal_child_fk FOREIGN KEY (tenant, PERIOD valid_at)
                REFERENCES temporal_parent (tenant, PERIOD valid_at)
        )",
    );
    exec(
        &engine,
        "INSERT INTO temporal_parent VALUES
            (7, '[2024-01-01,2024-01-10)'),
            (7, '[2024-01-10,2024-01-20)')",
    );
    assert_sqlstate(
        &engine,
        "INSERT INTO temporal_parent VALUES (7, '[2024-01-05,2024-01-12)')",
        "23P01",
    );
    exec(
        &engine,
        "INSERT INTO temporal_child VALUES (1, 7, '[2024-01-05,2024-01-15)')",
    );
    assert_sqlstate(
        &engine,
        "INSERT INTO temporal_child VALUES (2, 7, '[2024-01-05,2024-01-25)')",
        "23503",
    );
    assert_sqlstate(
        &engine,
        "DELETE FROM temporal_parent WHERE valid_at = '[2024-01-01,2024-01-10)'",
        "23503",
    );
    assert_sqlstate(
        &engine,
        "UPDATE temporal_parent SET valid_at = '[2024-01-12,2024-01-20)' WHERE valid_at = '[2024-01-10,2024-01-20)'",
        "23503",
    );
    let catalog = engine
        .sql(
            "SELECT conname, contype, conperiod FROM pg_constraint WHERE conname IN ('temporal_parent_pk', 'temporal_child_fk') ORDER BY conname",
            &[],
        )
        .unwrap();
    assert_eq!(catalog.rows.len(), 2);
    assert!(catalog
        .rows
        .iter()
        .all(|row| row["conperiod"] == Value::Bool(true)));
}

#[test]
fn alter_add_temporal_constraints_validates_existing_rows_and_survives_reopen() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("temporal.db");
    {
        let engine = Engine::open(&database).unwrap();
        exec(
            &engine,
            "CREATE TABLE alter_parent (tenant integer, valid_at daterange)",
        );
        exec(
            &engine,
            "INSERT INTO alter_parent VALUES (3, '[2024-01-01,2024-01-10)'), (3, '[2024-01-10,2024-01-20)')",
        );
        exec(
            &engine,
            "ALTER TABLE alter_parent ADD CONSTRAINT alter_parent_uq UNIQUE (tenant, valid_at WITHOUT OVERLAPS)",
        );
        exec(
            &engine,
            "CREATE TABLE alter_child (row_id integer PRIMARY KEY, tenant integer, valid_at daterange)",
        );
        exec(
            &engine,
            "INSERT INTO alter_child VALUES (1, 3, '[2024-01-05,2024-01-15)')",
        );
        exec(
            &engine,
            "ALTER TABLE alter_child ADD CONSTRAINT alter_child_fk FOREIGN KEY (tenant, PERIOD valid_at) REFERENCES alter_parent (tenant, PERIOD valid_at)",
        );
        exec(
            &engine,
            "CREATE TABLE durable_range_generated (
                id integer PRIMARY KEY,
                source int4range,
                lower_value integer GENERATED ALWAYS AS (lower(source)) STORED,
                wrapped int4multirange GENERATED ALWAYS AS (multirange(source)) STORED
            )",
        );
        exec(
            &engine,
            "INSERT INTO durable_range_generated (id, source) VALUES (1, '[1,5)')",
        );
    }
    let reopened = Engine::open(&database).unwrap();
    assert_sqlstate(
        &reopened,
        "INSERT INTO alter_parent VALUES (3, '[2024-01-03,2024-01-04)')",
        "23P01",
    );
    assert_sqlstate(
        &reopened,
        "INSERT INTO alter_child VALUES (2, 3, '[2024-01-19,2024-01-25)')",
        "23503",
    );
    let constraints = reopened
        .sql(
            "SELECT conname, conperiod FROM pg_constraint WHERE conname IN ('alter_parent_uq', 'alter_child_fk') ORDER BY conname",
            &[],
        )
        .unwrap();
    assert_eq!(constraints.rows.len(), 2);
    assert!(constraints
        .rows
        .iter()
        .all(|row| row["conperiod"] == Value::Bool(true)));
    exec(
        &reopened,
        "INSERT INTO durable_range_generated (id, source) VALUES (2, '[8,10)')",
    );
    let generated = reopened
        .sql(
            "SELECT lower_value, wrapped FROM durable_range_generated ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(generated.rows[0]["lower_value"], Value::Int(1));
    assert_eq!(generated.rows[0]["wrapped"], Value::Str("{[1,5)}".into()));
    assert_eq!(generated.rows[1]["lower_value"], Value::Int(8));
    assert_eq!(generated.rows[1]["wrapped"], Value::Str("{[8,10)}".into()));
}

#[test]
fn failed_temporal_constraint_additions_are_atomic_and_leave_no_catalog_rows() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE overlap_existing (tenant integer, valid_at daterange)",
    );
    exec(
        &engine,
        "INSERT INTO overlap_existing VALUES
            (1, '[2024-01-01,2024-01-10)'),
            (1, '[2024-01-05,2024-01-12)')",
    );
    assert_sqlstate(
        &engine,
        "ALTER TABLE overlap_existing ADD CONSTRAINT overlap_existing_uq UNIQUE (tenant, valid_at WITHOUT OVERLAPS)",
        "23P01",
    );
    let absent_key = engine
        .sql(
            "SELECT conname FROM pg_constraint WHERE conname = 'overlap_existing_uq'",
            &[],
        )
        .unwrap();
    assert!(absent_key.rows.is_empty());
    exec(
        &engine,
        "INSERT INTO overlap_existing VALUES (1, '[2024-01-06,2024-01-07)')",
    );

    exec(
        &engine,
        "CREATE TABLE covered_parent (
            tenant integer,
            valid_at daterange,
            UNIQUE (tenant, valid_at WITHOUT OVERLAPS)
        )",
    );
    exec(
        &engine,
        "INSERT INTO covered_parent VALUES (1, '[2024-01-01,2024-01-10)')",
    );
    exec(
        &engine,
        "CREATE TABLE uncovered_existing (id integer PRIMARY KEY, tenant integer, valid_at daterange)",
    );
    exec(
        &engine,
        "INSERT INTO uncovered_existing VALUES (1, 1, '[2024-01-05,2024-01-15)')",
    );
    assert_sqlstate(
        &engine,
        "ALTER TABLE uncovered_existing ADD CONSTRAINT uncovered_existing_fk FOREIGN KEY (tenant, PERIOD valid_at) REFERENCES covered_parent (tenant, PERIOD valid_at)",
        "23503",
    );
    let absent_foreign_key = engine
        .sql(
            "SELECT conname FROM pg_constraint WHERE conname = 'uncovered_existing_fk'",
            &[],
        )
        .unwrap();
    assert!(absent_foreign_key.rows.is_empty());
    exec(
        &engine,
        "INSERT INTO uncovered_existing VALUES (2, 1, '[2024-02-01,2024-02-02)')",
    );
}

#[test]
fn period_foreign_keys_require_matching_range_types_and_temporal_target_keys() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE ordinary_target (
            tenant integer,
            valid_at daterange,
            UNIQUE (tenant, valid_at)
        )",
    );
    assert_sqlstate(
        &engine,
        "CREATE TABLE missing_temporal_target (
            id integer PRIMARY KEY,
            tenant integer,
            valid_at daterange,
            FOREIGN KEY (tenant, PERIOD valid_at) REFERENCES ordinary_target (tenant, PERIOD valid_at)
        )",
        "42830",
    );

    exec(
        &engine,
        "CREATE TABLE date_target (
            tenant integer,
            valid_at daterange,
            UNIQUE (tenant, valid_at WITHOUT OVERLAPS)
        )",
    );
    assert_sqlstate(
        &engine,
        "CREATE TABLE mismatched_period_type (
            id integer PRIMARY KEY,
            tenant integer,
            valid_at int4range,
            FOREIGN KEY (tenant, PERIOD valid_at) REFERENCES date_target (tenant, PERIOD valid_at)
        )",
        "42804",
    );
    let absent_tables = engine
        .sql(
            "SELECT table_name FROM information_schema.tables WHERE table_name IN ('missing_temporal_target', 'mismatched_period_type')",
            &[],
        )
        .unwrap();
    assert!(absent_tables.rows.is_empty());
}

#[test]
fn alter_column_type_rewrites_atomically_and_preserves_temporal_dependencies() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE rewrite_values (id integer PRIMARY KEY, value text)",
    );
    exec(
        &engine,
        "INSERT INTO rewrite_values VALUES (1, '10'), (2, 'not-an-integer')",
    );
    assert!(engine
        .sql(
            "ALTER TABLE rewrite_values ALTER COLUMN value TYPE integer USING value::integer",
            &[],
        )
        .is_err());
    let unchanged = engine
        .sql(
            "SELECT value, pg_typeof(value)::text AS value_type FROM rewrite_values ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(unchanged.rows[0]["value"], Value::Str("10".into()));
    assert_eq!(
        unchanged.rows[1]["value"],
        Value::Str("not-an-integer".into())
    );
    assert_eq!(unchanged.rows[0]["value_type"], Value::Str("text".into()));

    exec(
        &engine,
        "CREATE TABLE alter_range (
            tenant integer,
            valid_at int4range,
            UNIQUE (tenant, valid_at WITHOUT OVERLAPS)
        )",
    );
    exec(&engine, "INSERT INTO alter_range VALUES (1, '[1,5)')");
    exec(
        &engine,
        "ALTER TABLE alter_range ALTER COLUMN valid_at TYPE int4multirange USING multirange(valid_at)",
    );
    let converted = engine
        .sql(
            "SELECT valid_at, pg_typeof(valid_at)::text AS ty FROM alter_range",
            &[],
        )
        .unwrap();
    assert_eq!(converted.rows[0]["valid_at"], Value::Str("{[1,5)}".into()));
    assert_eq!(converted.rows[0]["ty"], Value::Str("int4multirange".into()));

    exec(
        &engine,
        "CREATE TABLE dep_parent (
            tenant integer,
            valid_at daterange,
            PRIMARY KEY (tenant, valid_at WITHOUT OVERLAPS)
        )",
    );
    exec(
        &engine,
        "CREATE TABLE dep_child (
            row_id integer PRIMARY KEY,
            tenant integer,
            valid_at daterange,
            FOREIGN KEY (tenant, PERIOD valid_at) REFERENCES dep_parent (tenant, PERIOD valid_at)
        )",
    );
    assert_sqlstate(
        &engine,
        "ALTER TABLE dep_parent ALTER COLUMN valid_at TYPE datemultirange USING multirange(valid_at)",
        // PostgreSQL 18.4 oracle case `incompatible_parent_type_rewrite_rejected`
        // classifies a PERIOD dependency mismatch as datatype_mismatch, not
        // invalid_foreign_key.
        "42804",
    );
    let parent_type = engine
        .sql(
            "SELECT data_type FROM information_schema.columns WHERE table_name = 'dep_parent' AND column_name = 'valid_at'",
            &[],
        )
        .unwrap();
    assert_eq!(
        parent_type.rows[0]["data_type"],
        Value::Str("daterange".into())
    );

    exec(
        &engine,
        "CREATE TABLE generated_dependency (
            source int4range,
            lower_value integer GENERATED ALWAYS AS (lower(source)) STORED
        )",
    );
    assert!(engine
        .sql(
            "ALTER TABLE generated_dependency ALTER COLUMN source TYPE int4multirange USING multirange(source)",
            &[],
        )
        .is_err());
}
