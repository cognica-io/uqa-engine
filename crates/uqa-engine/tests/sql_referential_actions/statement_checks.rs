//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign keys are checked once a statement has written its rows, as `PostgreSQL`'s internal RI triggers are: a row may reference a row the statement writes later, a later row's unique or NOT NULL violation is reported first, a removed key that another row holds again satisfies `NO ACTION` but not `RESTRICT`, and a violation reports the key.

use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

/// The SQLSTATE, message and detail of the error `sql` fails with.
fn failure(engine: &Engine, sql: &str) -> (String, String, Option<String>) {
    let error = engine.sql(sql, &[]).unwrap_err();
    let detail = match &error {
        uqa_sql::SQLError::Diagnostic { detail, .. } => detail.clone(),
        _ => None,
    };
    (
        error.sqlstate().unwrap_or_default().to_string(),
        error.to_string(),
        detail,
    )
}

fn error(state: &str, message: &str, detail: &str) -> (String, String, Option<String>) {
    (state.into(), message.into(), Some(detail.into()))
}

fn missing(
    table: &str,
    constraint: &str,
    key: &str,
    referenced: &str,
) -> (String, String, Option<String>) {
    error(
        "23503",
        &format!("insert or update on table \"{table}\" violates foreign key constraint \"{constraint}\""),
        &format!("Key {key} is not present in table \"{referenced}\"."),
    )
}

fn referenced(
    table: &str,
    constraint: &str,
    referencing: &str,
    key: &str,
) -> (String, String, Option<String>) {
    error(
        "23503",
        &format!("update or delete on table \"{table}\" violates foreign key constraint \"{constraint}\" on table \"{referencing}\""),
        &format!("Key {key} is still referenced from table \"{referencing}\"."),
    )
}

#[test]
fn a_statement_checks_its_rows_against_foreign_keys_once_it_has_written_them() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE tree (id int PRIMARY KEY, parent int REFERENCES tree);
         INSERT INTO tree VALUES (2, 1), (1, NULL);
         CREATE TABLE p (id int PRIMARY KEY); INSERT INTO p VALUES (1);
         CREATE TABLE nn (id int PRIMARY KEY, pid int REFERENCES p, v int NOT NULL);
         CREATE TABLE mf (a int, b int, PRIMARY KEY (a, b));
         CREATE TABLE mc (id int PRIMARY KEY, a int, b int, FOREIGN KEY (a, b) REFERENCES mf MATCH FULL)",
    );
    for (sql, expected) in [
        (
            "INSERT INTO tree VALUES (4, 3), (4, NULL)",
            error(
                "23505",
                "duplicate key value violates unique constraint \"tree_pkey\"",
                "Key (id)=(4) already exists.",
            ),
        ),
        (
            "INSERT INTO tree VALUES (6, 5)",
            missing("tree", "tree_parent_fkey", "(parent)=(5)", "tree"),
        ),
        (
            "INSERT INTO nn VALUES (1, 99, 1), (2, 1, NULL)",
            error(
                "23502",
                "null value in column \"v\" of relation \"nn\" violates not-null constraint",
                "Failing row contains (2, 1, null).",
            ),
        ),
        (
            "INSERT INTO nn VALUES (3, 99, 1), (3, 1, 1)",
            error(
                "23505",
                "duplicate key value violates unique constraint \"nn_pkey\"",
                "Key (id)=(3) already exists.",
            ),
        ),
        (
            "INSERT INTO nn VALUES (5, 98, 1), (6, 99, 1)",
            missing("nn", "nn_pid_fkey", "(pid)=(98)", "p"),
        ),
        (
            "INSERT INTO mc VALUES (1, 1, NULL)",
            error(
                "23503",
                "insert or update on table \"mc\" violates foreign key constraint \"mc_a_b_fkey\"",
                "MATCH FULL does not allow mixing of null and nonnull key values.",
            ),
        ),
        (
            "INSERT INTO mc VALUES (2, 1, NULL), (2, 2, 2)",
            error(
                "23505",
                "duplicate key value violates unique constraint \"mc_pkey\"",
                "Key (id)=(2) already exists.",
            ),
        ),
        (
            "INSERT INTO mc VALUES (3, 1, 2)",
            missing("mc", "mc_a_b_fkey", "(a, b)=(1, 2)", "mf"),
        ),
    ] {
        assert_eq!(failure(&engine, sql), expected, "{sql}");
    }
    let rows = engine.sql("SELECT count(*) AS n FROM tree", &[]).unwrap();
    assert_eq!(rows.rows[0]["n"], uqa_core::Value::Int(2));
}

#[test]
fn a_removed_key_is_checked_when_the_statement_ends() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE sr (id int PRIMARY KEY, parent int REFERENCES sr ON DELETE RESTRICT);
         INSERT INTO sr VALUES (1, NULL), (2, 1);
         CREATE TABLE sn (id int PRIMARY KEY, parent int REFERENCES sn);
         INSERT INTO sn VALUES (1, NULL), (2, 1);
         CREATE TABLE tp (id text PRIMARY KEY); CREATE TABLE tc (pid text REFERENCES tp);
         INSERT INTO tp VALUES ('c'), ('a'); INSERT INTO tc VALUES ('c');
         CREATE TABLE rp (id text PRIMARY KEY); CREATE TABLE rc (pid text REFERENCES rp ON UPDATE RESTRICT);
         INSERT INTO rp VALUES ('c'), ('a'); INSERT INTO rc VALUES ('c');
         CREATE TABLE p (id int PRIMARY KEY); CREATE TABLE c (pid int REFERENCES p);
         INSERT INTO p VALUES (1); INSERT INTO c VALUES (1)",
    );
    // The statement removes the referencing rows with the referenced ones.
    exec(&engine, "DELETE FROM sr; DELETE FROM sn");
    // The key that the first row gave up the second row holds again by the end of the statement.
    exec(
        &engine,
        "UPDATE tp SET id = CASE id WHEN 'c' THEN 'e' ELSE 'c' END",
    );
    assert_eq!(
        failure(
            &engine,
            "UPDATE rp SET id = CASE id WHEN 'c' THEN 'e' ELSE 'c' END"
        ),
        error(
            "23001",
            "update or delete on table \"rp\" violates RESTRICT setting of foreign key constraint \"rc_pid_fkey\" on table \"rc\"",
            "Key (id)=(c) is referenced from table \"rc\".",
        )
    );
    assert_eq!(
        failure(&engine, "DELETE FROM p"),
        referenced("p", "c_pid_fkey", "c", "(id)=(1)")
    );
}

#[test]
fn foreign_key_checks_fire_among_user_after_triggers_in_name_order() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE px (id int PRIMARY KEY);
         CREATE TABLE fx (pid int REFERENCES px);
         CREATE FUNCTION fill() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO px VALUES (new.pid) ON CONFLICT DO NOTHING; RETURN NULL; END $$;
         CREATE TRIGGER \"A_fill\" AFTER INSERT ON fx FOR EACH ROW EXECUTE FUNCTION fill()",
    );
    // `A_fill` sorts before `RI_ConstraintTrigger_c_`, so it supplies the referenced row before the check.
    exec(&engine, "INSERT INTO fx VALUES (10)");
    exec(
        &engine,
        "DROP TRIGGER \"A_fill\" ON fx; CREATE TRIGGER a_fill AFTER INSERT ON fx FOR EACH ROW EXECUTE FUNCTION fill()",
    );
    assert_eq!(
        failure(&engine, "INSERT INTO fx VALUES (11)"),
        missing("fx", "fx_pid_fkey", "(pid)=(11)", "px")
    );
}

#[test]
fn a_moved_row_is_checked_in_its_new_partition_and_through_the_updated_table() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE mpk (id int PRIMARY KEY); INSERT INTO mpk VALUES (1), (2);
         CREATE TABLE mfk (k int, pid int REFERENCES mpk) PARTITION BY RANGE (k);
         CREATE TABLE mfk1 PARTITION OF mfk FOR VALUES FROM (0) TO (10);
         CREATE TABLE mfk2 PARTITION OF mfk FOR VALUES FROM (10) TO (20);
         INSERT INTO mfk VALUES (1, 1);
         CREATE TABLE rp (id int, k int, PRIMARY KEY (id, k)) PARTITION BY RANGE (k);
         CREATE TABLE rp1 PARTITION OF rp FOR VALUES FROM (0) TO (10);
         CREATE TABLE rp2 PARTITION OF rp FOR VALUES FROM (10) TO (20);
         CREATE TABLE rc (id int, k int, FOREIGN KEY (id, k) REFERENCES rp);
         INSERT INTO rp VALUES (1, 1); INSERT INTO rc VALUES (1, 1)",
    );
    assert_eq!(
        failure(&engine, "UPDATE mfk SET k = 11, pid = 9"),
        missing("mfk2", "mfk_pid_fkey", "(pid)=(9)", "mpk")
    );
    exec(&engine, "UPDATE mfk SET k = 11, pid = 2");
    assert_eq!(
        failure(&engine, "UPDATE rp SET k = 11"),
        referenced("rp", "rc_id_k_fkey", "rc", "(id, k)=(1, 1)")
    );
}

#[test]
fn every_write_path_checks_foreign_keys() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE p (id int PRIMARY KEY, v int); INSERT INTO p VALUES (1, 1), (2, 2);
         CREATE TABLE c (id int PRIMARY KEY, pid int REFERENCES p); INSERT INTO c VALUES (1, 1)",
    );
    let referencing = |key: i64| missing("c", "c_pid_fkey", &format!("(pid)=({key})"), "p");
    for (sql, expected) in [
        ("UPDATE c SET pid = 77 WHERE id = 1", referencing(77)),
        ("INSERT INTO c SELECT 2, 88", referencing(88)),
        ("INSERT INTO c SELECT g, g FROM generate_series(3, 4) g", referencing(3)),
        (
            "INSERT INTO c VALUES (1, 99) ON CONFLICT (id) DO UPDATE SET pid = excluded.pid",
            referencing(99),
        ),
        (
            "MERGE INTO c USING (SELECT 5 AS id, 66 AS pid) s ON c.id = s.id WHEN NOT MATCHED THEN INSERT VALUES (s.id, s.pid)",
            referencing(66),
        ),
        (
            "MERGE INTO c USING (SELECT 1 AS id, 55 AS pid) s ON c.id = s.id WHEN MATCHED THEN UPDATE SET pid = s.pid",
            referencing(55),
        ),
        (
            "UPDATE c SET pid = s.pid FROM (SELECT 1 AS id, 44 AS pid) s WHERE c.id = s.id",
            referencing(44),
        ),
        (
            "DELETE FROM p WHERE id = 1",
            referenced("p", "c_pid_fkey", "c", "(id)=(1)"),
        ),
        (
            "MERGE INTO p USING (SELECT 1 AS id) s ON p.id = s.id WHEN MATCHED THEN DELETE",
            referenced("p", "c_pid_fkey", "c", "(id)=(1)"),
        ),
        (
            "UPDATE p SET id = 10 WHERE id = 1",
            referenced("p", "c_pid_fkey", "c", "(id)=(1)"),
        ),
    ] {
        assert_eq!(failure(&engine, sql), expected, "{sql}");
    }
    // A cascaded update checks the keys the rows it rewrites hold.
    exec(
        &engine,
        "CREATE TABLE cc (id int PRIMARY KEY, pid int REFERENCES p ON UPDATE CASCADE);
         CREATE TABLE gg (gid int REFERENCES cc (id));
         INSERT INTO cc VALUES (7, 2); INSERT INTO gg VALUES (7);
         UPDATE p SET id = 20 WHERE id = 2",
    );
    assert_eq!(
        failure(&engine, "DELETE FROM cc WHERE id = 7"),
        referenced("cc", "gg_gid_fkey", "gg", "(id)=(7)")
    );
}

#[test]
fn deferred_foreign_key_violations_report_the_key() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE dp (id int PRIMARY KEY);
         CREATE TABLE dc (pid int REFERENCES dp DEFERRABLE INITIALLY DEFERRED);
         INSERT INTO dp VALUES (1)",
    );
    exec(&engine, "BEGIN; INSERT INTO dc VALUES (5)");
    assert_eq!(
        failure(&engine, "COMMIT"),
        missing("dc", "dc_pid_fkey", "(pid)=(5)", "dp")
    );
    // The insert commits on its own; in one query string it would join the transaction that BEGIN opens, and its own check would fail first.
    exec(&engine, "INSERT INTO dc VALUES (1)");
    exec(&engine, "BEGIN; DELETE FROM dp");
    assert_eq!(
        failure(&engine, "COMMIT"),
        referenced("dp", "dc_pid_fkey", "dc", "(id)=(1)")
    );
}

#[test]
fn temporal_foreign_keys_report_the_period_key() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE tpk (id int, valid daterange, PRIMARY KEY (id, valid WITHOUT OVERLAPS));
         CREATE TABLE tfk (id int, valid daterange, FOREIGN KEY (id, PERIOD valid) REFERENCES tpk (id, PERIOD valid));
         INSERT INTO tpk VALUES (1, '[2020-01-01,2021-01-01)');
         INSERT INTO tfk VALUES (1, '[2020-03-01,2020-04-01)')",
    );
    let still = referenced(
        "tpk",
        "tfk_id_valid_fkey",
        "tfk",
        "(id, valid)=(1, [2020-01-01,2021-01-01))",
    );
    for (sql, expected) in [
        ("DELETE FROM tpk", still.clone()),
        ("UPDATE tpk SET valid = '[2020-06-01,2021-01-01)'", still),
        (
            "INSERT INTO tfk VALUES (1, '[2022-01-01,2022-02-01)')",
            missing(
                "tfk",
                "tfk_id_valid_fkey",
                "(id, valid)=(1, [2022-01-01,2022-02-01))",
                "tpk",
            ),
        ),
    ] {
        assert_eq!(failure(&engine, sql), expected, "{sql}");
    }
    for (actions, clause) in [
        ("ON DELETE CASCADE", "ON DELETE"),
        ("ON UPDATE SET NULL", "ON UPDATE"),
        ("ON DELETE RESTRICT", "ON DELETE"),
        ("ON DELETE CASCADE ON UPDATE CASCADE", "ON UPDATE"),
    ] {
        let error = engine
            .sql(
                &format!("CREATE TABLE tfk2 (id int, valid daterange, FOREIGN KEY (id, PERIOD valid) REFERENCES tpk (id, PERIOD valid) {actions})"),
                &[],
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"), "{actions}");
        assert_eq!(
            error.to_string(),
            format!("unsupported {clause} action for foreign key constraint using PERIOD")
        );
    }
}
