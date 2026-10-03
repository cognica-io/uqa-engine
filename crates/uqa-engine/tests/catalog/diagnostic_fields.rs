//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Errors and notices report their detail and hint as fields of their own, and notices carry the SQLSTATE `PostgreSQL` 18 reports for them.

use uqa_engine::{Engine, SQLNotice};
use uqa_sql::SQLError;

type Fields = (String, String, Option<String>, Option<String>);

fn exec(engine: &Engine, sql: &str) {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

/// The SQLSTATE, message, detail and hint of the error that `sql` fails with.
fn error_fields(engine: &Engine, sql: &str) -> Fields {
    match engine.sql(sql, &[]).expect_err(sql) {
        SQLError::Diagnostic {
            sqlstate,
            message,
            detail,
            hint,
        } => (sqlstate, message, detail, hint),
        other => panic!("{sql}: {other:?} carries no diagnostic fields"),
    }
}

fn expected(sqlstate: &str, message: &str, detail: Option<&str>, hint: Option<&str>) -> Fields {
    (
        sqlstate.to_string(),
        message.to_string(),
        detail.map(str::to_string),
        hint.map(str::to_string),
    )
}

#[test]
fn errors_report_their_detail_and_hint_as_postgresql_does() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE up (id int PRIMARY KEY, v text);
         CREATE TABLE mt (id int, val int);
         INSERT INTO mt VALUES (1, 10);
         CREATE TABLE ms (id int, delta int);
         INSERT INTO ms VALUES (1, 1), (1, 2);
         CREATE TABLE pt (k int) PARTITION BY RANGE (k);
         CREATE TABLE pt1 PARTITION OF pt FOR VALUES FROM (0) TO (10);
         CREATE FUNCTION trf() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$;
         CREATE TABLE ct (a int);
         CREATE CONSTRAINT TRIGGER ctr AFTER INSERT ON ct DEFERRABLE FOR EACH ROW EXECUTE FUNCTION trf();
         CREATE TABLE rt (a int, b text);
         CREATE VIEW rv AS SELECT a, b FROM rt;
         CREATE TABLE ae (e int, x text);
         CREATE TABLE at2 (i int, y text)",
    );
    let cases = [
        (
            "INSERT INTO up VALUES (1, 'a'), (1, 'b') ON CONFLICT (id) DO UPDATE SET v = excluded.v",
            expected(
                "21000",
                "ON CONFLICT DO UPDATE command cannot affect row a second time",
                None,
                Some("Ensure that no rows proposed for insertion within the same command have duplicate constrained values."),
            ),
        ),
        (
            "MERGE INTO mt t USING ms s ON t.id = s.id WHEN MATCHED THEN UPDATE SET val = t.val + s.delta",
            expected(
                "21000",
                "MERGE command cannot affect row a second time",
                None,
                Some("Ensure that not more than one source row matches any one target row."),
            ),
        ),
        (
            "ALTER TABLE pt DETACH PARTITION pt1 FINALIZE",
            expected(
                "55000",
                "cannot complete detaching partition \"pt1\"",
                Some("There's no pending concurrent detach."),
                None,
            ),
        ),
        (
            "ALTER TABLE ct DROP CONSTRAINT ctr",
            expected(
                "2BP01",
                "cannot drop constraint ctr on table ct because trigger ctr on table ct requires it",
                None,
                Some("You can drop trigger ctr on table ct instead."),
            ),
        ),
        (
            "DROP RULE \"_RETURN\" ON rv",
            expected(
                "2BP01",
                "cannot drop rule _RETURN on view rv because view rv requires it",
                None,
                Some("You can drop view rv instead."),
            ),
        ),
        (
            "CREATE RULE rr AS ON INSERT TO rv DO INSTEAD INSERT INTO rt VALUES (new.a, new.b) RETURNING rt.b, rt.a",
            expected(
                "42P17",
                "RETURNING list's entry 1 has different type from column \"a\"",
                Some("RETURNING list entry has type text, but column has type integer."),
                None,
            ),
        ),
        (
            "CREATE RULE ai AS ON UPDATE TO ae DO INSTEAD INSERT INTO at2 VALUES (NEW.e, NEW.x) RETURNING WITH (NEW AS action_new) NEW.*",
            expected(
                "42P01",
                "invalid reference to FROM-clause entry for table \"new\"",
                Some("There is an entry for table \"new\", but it cannot be referenced from this part of the query."),
                None,
            ),
        ),
        (
            "SET session_replication_role = rep",
            expected(
                "22023",
                "invalid value for parameter \"session_replication_role\": \"rep\"",
                None,
                Some("Available values: origin, replica, local."),
            ),
        ),
    ];
    for (sql, fields) in cases {
        assert_eq!(error_fields(&engine, sql), fields, "{sql}");
    }
}

#[test]
fn attached_partition_columns_are_checked_in_their_order_before_the_parent_columns() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE ap (k int, v int) PARTITION BY RANGE (k);
         CREATE TABLE ai1 (k int, v int GENERATED ALWAYS AS IDENTITY);
         CREATE TABLE ax1 (k int, v int, extra int);
         CREATE TABLE ax2 (extra int, k int);
         CREATE TABLE ax3 (k int, extra int GENERATED ALWAYS AS IDENTITY, v int);
         CREATE TABLE ax4 (other int, k int GENERATED ALWAYS AS IDENTITY, v int)",
    );
    let identity = |table: &str, column: &str| {
        expected(
            "55000",
            &format!("table \"{table}\" being attached contains an identity column \"{column}\""),
            Some("The new partition may not contain an identity column."),
            None,
        )
    };
    let extra = |table: &str, column: &str| {
        expected(
            "42804",
            &format!("table \"{table}\" contains column \"{column}\" not found in parent \"ap\""),
            Some("The new partition may contain only the columns present in parent."),
            None,
        )
    };
    for (table, fields) in [
        ("ai1", identity("ai1", "v")),
        ("ax1", extra("ax1", "extra")),
        ("ax2", extra("ax2", "extra")),
        ("ax3", identity("ax3", "extra")),
        ("ax4", extra("ax4", "other")),
    ] {
        assert_eq!(
            error_fields(
                &engine,
                &format!("ALTER TABLE ap ATTACH PARTITION {table} FOR VALUES FROM (0) TO (10)")
            ),
            fields,
            "{table}"
        );
    }
}

#[test]
fn notices_carry_the_sqlstate_postgresql_reports() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE t (a int);
         CREATE SEQUENCE s;
         CREATE ROLE grant_holder;
         CREATE ROLE grant_target;
         GRANT SELECT ON t TO grant_holder WITH GRANT OPTION",
    );
    engine.take_sql_notices();
    let cases = [
        (
            "CREATE TABLE IF NOT EXISTS t (a int)",
            vec![SQLNotice::notice("relation \"t\" already exists, skipping").with_sqlstate("42P07")],
        ),
        (
            "ALTER TABLE t ADD COLUMN IF NOT EXISTS a int",
            vec![
                SQLNotice::notice("column \"a\" of relation \"t\" already exists, skipping")
                    .with_sqlstate("42701"),
            ],
        ),
        (
            "CREATE SCHEMA IF NOT EXISTS public",
            vec![SQLNotice::notice("schema \"public\" already exists, skipping").with_sqlstate("42P06")],
        ),
        (
            "CREATE INDEX IF NOT EXISTS t ON t (a)",
            vec![SQLNotice::notice("relation \"t\" already exists, skipping").with_sqlstate("42P07")],
        ),
        (
            "CREATE SEQUENCE IF NOT EXISTS t",
            vec![SQLNotice::notice("relation \"t\" already exists, skipping").with_sqlstate("42P07")],
        ),
        (
            "DROP TABLE IF EXISTS missing",
            vec![SQLNotice::notice("table \"missing\" does not exist, skipping")],
        ),
        (
            "COMMIT",
            vec![SQLNotice::warning("there is no transaction in progress").with_sqlstate("25P01")],
        ),
        (
            "SET CONSTRAINTS ALL DEFERRED",
            vec![
                SQLNotice::warning("SET CONSTRAINTS can only be used in transaction blocks")
                    .with_sqlstate("25P01"),
            ],
        ),
        (
            "GRANT INSERT ON TABLE s TO grant_target",
            vec![SQLNotice::warning(
                "sequence \"s\" only supports USAGE, SELECT, and UPDATE privileges",
            )
            .with_sqlstate("0LP01")],
        ),
        (
            "SET ROLE grant_holder; GRANT INSERT ON t TO grant_target; REVOKE INSERT ON t FROM grant_target; RESET ROLE",
            vec![
                SQLNotice::warning("no privileges were granted for \"t\"").with_sqlstate("01007"),
                SQLNotice::warning("no privileges could be revoked for \"t\"").with_sqlstate("01006"),
            ],
        ),
        (
            "DO $$ BEGIN RAISE NOTICE division_by_zero; RAISE NOTICE SQLSTATE '22012'; END $$",
            vec![
                SQLNotice::notice("division_by_zero").with_sqlstate("22012"),
                SQLNotice::notice("22012").with_sqlstate("22012"),
            ],
        ),
        (
            "DO $$ BEGIN RAISE WARNING 'w'; RAISE INFO 'i'; RAISE NOTICE 'n'; END $$",
            vec![
                SQLNotice::warning("w"),
                SQLNotice::new(uqa_engine::NoticeLevel::Info, "i"),
                SQLNotice::notice("n"),
            ],
        ),
    ];
    for (sql, notices) in cases {
        exec(&engine, sql);
        assert_eq!(engine.take_sql_notices(), notices, "{sql}");
    }
}
