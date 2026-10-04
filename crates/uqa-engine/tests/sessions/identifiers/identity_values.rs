//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Identity columns take the values `PostgreSQL` writes to them: `OVERRIDING SYSTEM VALUE` and `OVERRIDING USER VALUE` select what an insert writes, a NULL a statement supplies is a value, `DEFAULT` draws the next sequence value in an update, and a `GENERATED ALWAYS` column takes no other assigned value. The checks precede every row, so a statement whose source yields none is rejected as well. The expected values are those of `PostgreSQL` 18.4.

use super::*;

/// Run `scenario` on a memory engine and on a native `SQLite` engine.
fn on_memory_and_native(scenario: impl Fn(&Engine, &str)) {
    scenario(&Engine::new(), "memory");
    let directory = tempfile::tempdir().unwrap();
    scenario(
        &open(Layout::Native, &directory.path().join("identity-values.db")),
        "native",
    );
}

/// The SQLSTATE, message, detail and hint of the error `statement` fails with.
fn diagnostic(
    engine: &Engine,
    statement: &str,
) -> (String, String, Option<String>, Option<String>) {
    match engine.sql(statement, &[]) {
        Err(uqa_sql::SQLError::Diagnostic {
            sqlstate,
            message,
            detail,
            hint,
        }) => (sqlstate, message, detail, hint),
        other => panic!("{statement}: expected a diagnostic, got {other:?}"),
    }
}

fn assert_insert_rejected(engine: &Engine, label: &str, statement: &str) {
    assert_eq!(
        diagnostic(engine, statement),
        (
            "428C9".into(),
            "cannot insert a non-DEFAULT value into column \"id\"".into(),
            Some("Column \"id\" is an identity column defined as GENERATED ALWAYS.".into()),
            Some("Use OVERRIDING SYSTEM VALUE to override.".into()),
        ),
        "{label}: {statement}"
    );
}

fn assert_update_rejected(engine: &Engine, label: &str, statement: &str) {
    assert_eq!(
        diagnostic(engine, statement),
        (
            "428C9".into(),
            "column \"id\" can only be updated to DEFAULT".into(),
            Some("Column \"id\" is an identity column defined as GENERATED ALWAYS.".into()),
            None,
        ),
        "{label}: {statement}"
    );
}

fn listed(engine: &Engine, table: &str) -> Vec<String> {
    rows(
        engine,
        &format!("SELECT id, v FROM {table} ORDER BY v"),
        &["id", "v"],
    )
}

#[test]
fn overriding_selects_the_value_an_insert_writes_to_an_identity_column() {
    on_memory_and_native(|engine, label| {
        run(
            engine,
            "CREATE TABLE always_rows (id integer GENERATED ALWAYS AS IDENTITY, v integer)",
        );
        run(engine, "INSERT INTO always_rows (v) VALUES (1)");
        for statement in [
            "INSERT INTO always_rows (id, v) VALUES (10, 2)",
            "INSERT INTO always_rows (id, v) VALUES (NULL, 2)",
            "INSERT INTO always_rows (id, v) SELECT 10, 2 WHERE false",
            "INSERT INTO always_rows VALUES (10, 2)",
            "MERGE INTO always_rows USING (VALUES (90, 9)) AS source(id, v) ON false
             WHEN NOT MATCHED THEN INSERT (id, v) VALUES (source.id, source.v)",
            "MERGE INTO always_rows USING (SELECT 90 AS id, 9 AS v WHERE false) AS source ON false
             WHEN NOT MATCHED THEN INSERT (id, v) VALUES (source.id, source.v)",
        ] {
            assert_insert_rejected(engine, label, statement);
        }
        run(
            engine,
            "INSERT INTO always_rows (id, v) VALUES (DEFAULT, 2)",
        );
        run(
            engine,
            "INSERT INTO always_rows (id, v) OVERRIDING SYSTEM VALUE VALUES (10, 3)",
        );
        assert_eq!(
            state(
                engine,
                "INSERT INTO always_rows (id, v) OVERRIDING SYSTEM VALUE VALUES (NULL, 3)"
            )
            .as_deref(),
            Some("23502"),
            "{label}"
        );
        run(
            engine,
            "INSERT INTO always_rows (id, v) OVERRIDING USER VALUE VALUES (20, 4)",
        );
        run(
            engine,
            "INSERT INTO always_rows (id, v) OVERRIDING SYSTEM VALUE SELECT 50, 5",
        );
        run(
            engine,
            "INSERT INTO always_rows (id, v) OVERRIDING USER VALUE SELECT 60, 6",
        );
        run(
            engine,
            "MERGE INTO always_rows USING (VALUES (70, 7)) AS source(id, v) ON false
             WHEN NOT MATCHED THEN INSERT (id, v) OVERRIDING SYSTEM VALUE VALUES (source.id, source.v)",
        );
        run(
            engine,
            "MERGE INTO always_rows USING (VALUES (80, 8)) AS source(id, v) ON false
             WHEN NOT MATCHED THEN INSERT (id, v) OVERRIDING USER VALUE VALUES (source.id, source.v)",
        );
        assert_eq!(
            listed(engine, "always_rows"),
            ["1|1", "2|2", "10|3", "3|4", "50|5", "4|6", "70|7", "5|8"],
            "{label}"
        );

        run(
            engine,
            "CREATE TABLE default_rows (id integer GENERATED BY DEFAULT AS IDENTITY, v integer)",
        );
        assert_eq!(
            state(engine, "INSERT INTO default_rows (id, v) VALUES (NULL, 1)").as_deref(),
            Some("23502"),
            "{label}"
        );
        run(
            engine,
            "INSERT INTO default_rows (id, v) OVERRIDING USER VALUE VALUES (20, 1)",
        );
        run(
            engine,
            "INSERT INTO default_rows (id, v) OVERRIDING SYSTEM VALUE VALUES (30, 2)",
        );
        run(engine, "INSERT INTO default_rows (id, v) VALUES (40, 3)");
        run(engine, "UPDATE default_rows SET id = 50 WHERE v = 3");
        run(engine, "UPDATE default_rows SET id = DEFAULT WHERE v = 2");
        assert_eq!(
            listed(engine, "default_rows"),
            ["1|1", "2|2", "50|3"],
            "{label}"
        );

        // A SERIAL column is not an identity column, so neither clause changes its value.
        run(engine, "CREATE TABLE serial_rows (id serial, v integer)");
        run(
            engine,
            "INSERT INTO serial_rows (id, v) OVERRIDING USER VALUE VALUES (20, 1)",
        );
        run(
            engine,
            "INSERT INTO serial_rows (id, v) OVERRIDING SYSTEM VALUE VALUES (30, 2)",
        );
        assert_eq!(listed(engine, "serial_rows"), ["20|1", "30|2"], "{label}");
    });
}

#[test]
fn default_draws_the_next_identity_value_and_an_always_column_takes_no_other() {
    on_memory_and_native(|engine, label| {
        run(
            engine,
            "CREATE TABLE always_rows (id integer GENERATED ALWAYS AS IDENTITY, v integer);
             CREATE UNIQUE INDEX always_rows_v ON always_rows (v)",
        );
        run(engine, "INSERT INTO always_rows (v) VALUES (1), (2)");
        for statement in [
            "UPDATE always_rows SET id = 100 WHERE v = 1",
            "UPDATE always_rows SET id = 100 WHERE false",
            "UPDATE always_rows SET id = id WHERE v = 1",
            "INSERT INTO always_rows (v) VALUES (1) ON CONFLICT (v) DO UPDATE SET id = 77",
            "MERGE INTO always_rows USING (VALUES (1)) AS source(v) ON always_rows.v = source.v
             WHEN MATCHED THEN UPDATE SET id = 88",
            "MERGE INTO always_rows USING (SELECT 1 AS v WHERE false) AS source ON always_rows.v = source.v
             WHEN MATCHED THEN UPDATE SET id = 88",
        ] {
            assert_update_rejected(engine, label, statement);
        }
        // Each DEFAULT draws a value: 3 for the UPDATE, 4 for the row the conflicting INSERT proposes and 5 for its update, and 6 for the MERGE.
        run(engine, "UPDATE always_rows SET id = DEFAULT WHERE v = 1");
        assert_eq!(listed(engine, "always_rows"), ["3|1", "2|2"], "{label}");
        run(
            engine,
            "INSERT INTO always_rows (v) VALUES (1) ON CONFLICT (v) DO UPDATE SET id = DEFAULT",
        );
        assert_eq!(listed(engine, "always_rows"), ["5|1", "2|2"], "{label}");
        run(
            engine,
            "MERGE INTO always_rows USING (VALUES (1)) AS source(v) ON always_rows.v = source.v
             WHEN MATCHED THEN UPDATE SET id = DEFAULT",
        );
        assert_eq!(listed(engine, "always_rows"), ["6|1", "2|2"], "{label}");
    });
}

#[test]
fn views_and_copy_write_identity_values_as_postgresql_does() {
    on_memory_and_native(|engine, label| {
        run(
            engine,
            "CREATE TABLE always_rows (id integer GENERATED ALWAYS AS IDENTITY, v integer);
             CREATE VIEW always_view AS SELECT id, v FROM always_rows",
        );
        run(engine, "INSERT INTO always_view (v) VALUES (1)");
        assert_insert_rejected(
            engine,
            label,
            "INSERT INTO always_view (id, v) VALUES (99, 9)",
        );
        run(
            engine,
            "INSERT INTO always_view (id, v) OVERRIDING SYSTEM VALUE VALUES (99, 2)",
        );
        run(
            engine,
            "INSERT INTO always_view (id, v) OVERRIDING USER VALUE VALUES (98, 3)",
        );
        // COPY FROM writes the value its input supplies, as `OVERRIDING SYSTEM VALUE` does.
        assert_eq!(
            engine
                .copy_from("COPY always_rows (id, v) FROM STDIN", "200\t4\n".as_bytes())
                .unwrap(),
            1,
            "{label}"
        );
        assert_eq!(
            engine
                .copy_from("COPY always_rows (v) FROM STDIN", "5\n".as_bytes())
                .unwrap(),
            1,
            "{label}"
        );
        assert_eq!(
            listed(engine, "always_rows"),
            ["1|1", "99|2", "2|3", "200|4", "3|5"],
            "{label}"
        );
    });
}
