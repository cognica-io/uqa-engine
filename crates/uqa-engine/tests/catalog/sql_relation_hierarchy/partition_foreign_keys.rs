//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Each partition of a partitioned referencing table holds a foreign key for each of the partitioned table's foreign keys: an equivalent foreign key of its own that it attaches, or a copy, as `PostgreSQL`'s `addFkRecurseReferencing` and `CloneFkReferencing` give it one, validated partition by partition.

use super::{exec, Engine, Value};

fn constraints(engine: &Engine, sql: &str) -> Vec<(String, String)> {
    engine
        .sql(sql, &[])
        .unwrap()
        .rows
        .into_iter()
        .map(|row| {
            let text = |column: &str| match &row[column] {
                Value::Str(text) => text.clone(),
                other => panic!("unexpected {column}: {other:?}"),
            };
            (text("conname"), text("conrelid"))
        })
        .collect()
}

fn foreign_keys(engine: &Engine) -> Vec<(String, String)> {
    let mut rows = constraints(
        engine,
        "SELECT conname, conrelid::regclass::text AS conrelid FROM pg_constraint WHERE contype = 'f'",
    );
    rows.sort_by(|left, right| (&left.1, &left.0).cmp(&(&right.1, &right.0)));
    rows
}

fn validated(engine: &Engine) -> Vec<(String, bool)> {
    let mut rows = engine
        .sql(
            "SELECT conrelid::regclass::text AS conrelid, convalidated FROM pg_constraint WHERE contype = 'f'",
            &[],
        )
        .unwrap()
        .rows
        .into_iter()
        .map(|row| {
            let Value::Str(table) = &row["conrelid"] else {
                panic!("unexpected conrelid");
            };
            let Value::Bool(validated) = row["convalidated"] else {
                panic!("unexpected convalidated");
            };
            (table.clone(), validated)
        })
        .collect::<Vec<_>>();
    rows.sort();
    rows
}

fn pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(name, table)| ((*name).to_string(), (*table).to_string()))
        .collect()
}

fn assert_violation(engine: &Engine, sql: &str, message: &str) {
    let error = engine.sql(sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("23503"), "{error}");
    assert!(error.to_string().contains(message), "{error}");
}

fn assert_error(engine: &Engine, sql: &str, sqlstate: &str, message: &str) {
    let error = engine.sql(sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some(sqlstate), "{error}");
    assert!(error.to_string().contains(message), "{error}");
}

fn referencing_tree(engine: &Engine) {
    for sql in [
        "CREATE TABLE pk (a integer PRIMARY KEY)",
        "INSERT INTO pk VALUES (1)",
        "CREATE TABLE fk (a integer, b integer) PARTITION BY RANGE (b)",
        "CREATE TABLE fk1 PARTITION OF fk FOR VALUES FROM (0) TO (10)",
        "CREATE TABLE fk2 PARTITION OF fk FOR VALUES FROM (10) TO (20) PARTITION BY RANGE (b)",
        "CREATE TABLE fk21 PARTITION OF fk2 FOR VALUES FROM (10) TO (15)",
        "ALTER TABLE fk21 ADD CONSTRAINT fk_a_fkey CHECK (a > 0)",
        "INSERT INTO fk VALUES (1, 5), (1, 12)",
    ] {
        exec(engine, sql);
    }
}

#[test]
fn adding_a_foreign_key_copies_it_to_the_existing_partitions() {
    let engine = Engine::new();
    referencing_tree(&engine);
    exec(
        &engine,
        "ALTER TABLE fk ADD CONSTRAINT fk_a_fkey FOREIGN KEY (a) REFERENCES pk",
    );
    // A partition that already uses the name receives the name with the first free suffix.
    assert_eq!(
        foreign_keys(&engine),
        pairs(&[
            ("fk_a_fkey", "fk"),
            ("fk_a_fkey", "fk1"),
            ("fk_a_fkey", "fk2"),
            ("fk_a_fkey_1", "fk21"),
        ])
    );
    assert_violation(
        &engine,
        "INSERT INTO fk VALUES (2, 12)",
        "insert or update on table \"fk21\" violates foreign key constraint \"fk_a_fkey_1\"",
    );
    assert_violation(
        &engine,
        "INSERT INTO fk1 VALUES (3, 5)",
        "insert or update on table \"fk1\" violates foreign key constraint \"fk_a_fkey\"",
    );
    exec(&engine, "ALTER TABLE fk DROP CONSTRAINT fk_a_fkey");
    assert!(foreign_keys(&engine).is_empty());
}

#[test]
fn adding_and_validating_a_foreign_key_read_the_rows_of_every_leaf_partition() {
    let engine = Engine::new();
    referencing_tree(&engine);
    exec(&engine, "INSERT INTO fk VALUES (2, 13)");
    assert_violation(
        &engine,
        "ALTER TABLE fk ADD CONSTRAINT fk_a_fkey FOREIGN KEY (a) REFERENCES pk",
        "insert or update on table \"fk21\" violates foreign key constraint \"fk_a_fkey_1\"",
    );
    assert!(foreign_keys(&engine).is_empty());
    exec(
        &engine,
        "ALTER TABLE fk ADD CONSTRAINT fk_a_fkey FOREIGN KEY (a) REFERENCES pk NOT VALID",
    );
    exec(&engine, "ALTER TABLE fk1 VALIDATE CONSTRAINT fk_a_fkey");
    assert_eq!(
        validated(&engine),
        [
            ("fk".to_string(), false),
            ("fk1".to_string(), true),
            ("fk2".to_string(), false),
            ("fk21".to_string(), false),
        ]
    );
    assert_violation(
        &engine,
        "ALTER TABLE fk VALIDATE CONSTRAINT fk_a_fkey",
        "insert or update on table \"fk21\" violates foreign key constraint \"fk_a_fkey_1\"",
    );
    exec(&engine, "DELETE FROM fk WHERE a = 2");
    exec(&engine, "ALTER TABLE fk VALIDATE CONSTRAINT fk_a_fkey");
    assert!(validated(&engine).iter().all(|(_, validated)| *validated));
}

#[test]
fn adding_a_foreign_key_attaches_the_equivalent_foreign_keys_of_partitions() {
    let engine = Engine::new();
    for sql in [
        "CREATE TABLE pk (a integer PRIMARY KEY)",
        "CREATE TABLE fk (a integer, b integer) PARTITION BY RANGE (b)",
        "CREATE TABLE fk1 PARTITION OF fk FOR VALUES FROM (0) TO (10) PARTITION BY RANGE (b)",
        "ALTER TABLE fk1 ADD CONSTRAINT fk1_own FOREIGN KEY (a) REFERENCES pk",
        "CREATE TABLE fk11 PARTITION OF fk1 FOR VALUES FROM (0) TO (5)",
        "CREATE TABLE fk2 PARTITION OF fk FOR VALUES FROM (10) TO (20)",
        "ALTER TABLE fk2 ADD CONSTRAINT fk2_deferrable FOREIGN KEY (a) REFERENCES pk DEFERRABLE",
        "ALTER TABLE fk ADD CONSTRAINT fk_a_fkey FOREIGN KEY (a) REFERENCES pk",
    ] {
        exec(&engine, sql);
    }
    // fk1 attaches its own foreign key, whose copy on fk11 follows it; fk2's differs in deferrability and receives a copy.
    assert_eq!(
        foreign_keys(&engine),
        pairs(&[
            ("fk_a_fkey", "fk"),
            ("fk1_own", "fk1"),
            ("fk1_own", "fk11"),
            ("fk2_deferrable", "fk2"),
            ("fk_a_fkey", "fk2"),
        ])
    );
    for (table, name) in [("fk1", "fk1_own"), ("fk11", "fk1_own")] {
        assert_error(
            &engine,
            &format!("ALTER TABLE {table} DROP CONSTRAINT {name}"),
            "42P16",
            &format!("cannot drop inherited constraint \"{name}\" of relation \"{table}\""),
        );
    }
    exec(&engine, "ALTER TABLE fk2 DROP CONSTRAINT fk2_deferrable");
    exec(&engine, "ALTER TABLE fk DROP CONSTRAINT fk_a_fkey");
    assert!(foreign_keys(&engine).is_empty());
}

#[test]
fn attaching_a_partition_validates_its_copies_and_attaches_equivalent_foreign_keys() {
    let engine = Engine::new();
    for sql in [
        "CREATE TABLE pk (a integer PRIMARY KEY)",
        "INSERT INTO pk VALUES (1)",
        "CREATE TABLE fk (a integer, b integer) PARTITION BY RANGE (b)",
        "ALTER TABLE fk ADD CONSTRAINT fk_a_fkey FOREIGN KEY (a) REFERENCES pk NOT VALID",
        "CREATE TABLE fk1 (a integer, b integer)",
        "INSERT INTO fk1 VALUES (7, 5)",
    ] {
        exec(&engine, sql);
    }
    // A copy's rows are validated although the parent's foreign key is not valid.
    assert_violation(
        &engine,
        "ALTER TABLE fk ATTACH PARTITION fk1 FOR VALUES FROM (0) TO (10)",
        "insert or update on table \"fk1\" violates foreign key constraint \"fk_a_fkey\"",
    );
    // An equivalent foreign key attaches without validation under a parent's foreign key that is not valid.
    for sql in [
        "DELETE FROM fk1",
        "INSERT INTO fk1 VALUES (9, 5)",
        "ALTER TABLE fk1 ADD CONSTRAINT fk1_own FOREIGN KEY (a) REFERENCES pk NOT VALID",
        "ALTER TABLE fk ATTACH PARTITION fk1 FOR VALUES FROM (0) TO (10)",
    ] {
        exec(&engine, sql);
    }
    assert_eq!(
        foreign_keys(&engine),
        pairs(&[("fk_a_fkey", "fk"), ("fk1_own", "fk1")])
    );
    assert_error(
        &engine,
        "ALTER TABLE fk1 DROP CONSTRAINT fk1_own",
        "42P16",
        "cannot drop inherited constraint \"fk1_own\" of relation \"fk1\"",
    );
    // Under a valid foreign key, an equivalent foreign key that is not valid has its rows validated.
    for sql in [
        "CREATE TABLE gk (a integer, b integer) PARTITION BY RANGE (b)",
        "ALTER TABLE gk ADD CONSTRAINT gk_a_fkey FOREIGN KEY (a) REFERENCES pk",
        "CREATE TABLE gk1 (a integer, b integer)",
        "INSERT INTO gk1 VALUES (8, 5)",
        "ALTER TABLE gk1 ADD CONSTRAINT gk1_own FOREIGN KEY (a) REFERENCES pk NOT VALID",
    ] {
        exec(&engine, sql);
    }
    assert_violation(
        &engine,
        "ALTER TABLE gk ATTACH PARTITION gk1 FOR VALUES FROM (0) TO (10)",
        "insert or update on table \"gk1\" violates foreign key constraint \"gk1_own\"",
    );
    exec(&engine, "DELETE FROM gk1");
    exec(
        &engine,
        "ALTER TABLE gk ATTACH PARTITION gk1 FOR VALUES FROM (0) TO (10)",
    );
    assert!(
        validated(&engine).contains(&("gk1".to_string(), true)),
        "{:?}",
        validated(&engine)
    );
    // A foreign key that differs from the parent's in enforceability alone is an error.
    for sql in [
        "CREATE TABLE y (a integer, b integer)",
        "ALTER TABLE y ADD CONSTRAINT y_own FOREIGN KEY (a) REFERENCES pk NOT ENFORCED",
    ] {
        exec(&engine, sql);
    }
    assert_error(
        &engine,
        "ALTER TABLE fk ATTACH PARTITION y FOR VALUES FROM (10) TO (20)",
        "42P16",
        "constraint \"fk_a_fkey\" enforceability conflicts with constraint \"y_own\" on relation \"y\"",
    );
}

#[test]
fn a_partition_constraint_named_like_a_parent_foreign_key_is_rejected() {
    let engine = Engine::new();
    for sql in [
        "CREATE TABLE pk (a integer PRIMARY KEY)",
        "CREATE TABLE fk (a integer REFERENCES pk, b integer) PARTITION BY RANGE (b)",
        "CREATE TABLE fk2 (a integer, b integer, CONSTRAINT fk_a_fkey CHECK (a > 0))",
        "ALTER TABLE fk ATTACH PARTITION fk2 FOR VALUES FROM (10) TO (20)",
    ] {
        exec(&engine, sql);
    }
    assert_error(
        &engine,
        "CREATE TABLE fk1 PARTITION OF fk (CONSTRAINT fk_a_fkey CHECK (a > 0)) FOR VALUES FROM (0) TO (10)",
        "42710",
        "constraint \"fk_a_fkey\" for relation \"fk1\" already exists",
    );
    assert_eq!(
        foreign_keys(&engine),
        pairs(&[("fk_a_fkey", "fk"), ("fk_a_fkey_1", "fk2")])
    );
}

fn rewrite_stored_constraints(
    path: &std::path::Path,
    mut rewrite: impl FnMut(&str, &mut uqa_sql::ast::TableConstraintSet),
) {
    let connection = rusqlite::Connection::open(path).unwrap();
    let rows = connection
        .prepare("SELECT schema_name, relation_name, constraints FROM _tables")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for (schema, relation, constraints) in rows {
        let mut constraints: uqa_sql::ast::TableConstraintSet =
            serde_json::from_str(&constraints).unwrap();
        rewrite(&relation, &mut constraints);
        connection
            .execute(
                "UPDATE _tables SET constraints = ?1 WHERE schema_name = ?2 AND relation_name = ?3",
                rusqlite::params![
                    serde_json::to_string(&constraints).unwrap(),
                    schema,
                    relation
                ],
            )
            .unwrap();
    }
}

#[test]
fn reopening_gives_partitions_the_foreign_key_copies_they_lack() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("partition-foreign-key-repair.db");
    {
        let engine = crate::native_storage::legacy_engine(&path);
        for sql in [
            "CREATE TABLE pk (a integer PRIMARY KEY)",
            "INSERT INTO pk VALUES (1)",
            "CREATE TABLE fk (a integer, b integer) PARTITION BY RANGE (b)",
            "CREATE TABLE fk1 PARTITION OF fk FOR VALUES FROM (0) TO (10)",
            "INSERT INTO fk VALUES (1, 5), (7, 6)",
            "ALTER TABLE fk ADD CONSTRAINT fk_a_fkey FOREIGN KEY (a) REFERENCES pk NOT VALID",
        ] {
            exec(&engine, sql);
        }
    }
    // Releases that did not recurse ADD FOREIGN KEY left the partition without a copy, and their VALIDATE CONSTRAINT marked the foreign key valid without reading the partition.
    rewrite_stored_constraints(&path, |relation, constraints| match relation {
        "fk" => constraints.foreign_keys[0].validated = true,
        "fk1" => constraints.foreign_keys.clear(),
        _ => {}
    });
    let reopened = Engine::open(&path).unwrap();
    assert_eq!(
        foreign_keys(&reopened),
        pairs(&[("fk_a_fkey", "fk"), ("fk_a_fkey", "fk1")])
    );
    assert_eq!(
        validated(&reopened),
        [("fk".to_string(), false), ("fk1".to_string(), false)]
    );
    assert_violation(
        &reopened,
        "INSERT INTO fk VALUES (8, 5)",
        "insert or update on table \"fk1\" violates foreign key constraint \"fk_a_fkey\"",
    );
    assert_violation(
        &reopened,
        "ALTER TABLE fk VALIDATE CONSTRAINT fk_a_fkey",
        "insert or update on table \"fk1\" violates foreign key constraint \"fk_a_fkey\"",
    );
    exec(&reopened, "DELETE FROM fk WHERE a = 7");
    exec(&reopened, "ALTER TABLE fk VALIDATE CONSTRAINT fk_a_fkey");
    assert!(validated(&reopened).iter().all(|(_, validated)| *validated));
}

fn diagnostic(error: uqa_sql::SQLError) -> (String, String, Option<String>, Option<String>) {
    match error {
        uqa_sql::SQLError::Diagnostic {
            sqlstate,
            message,
            detail,
            hint,
        } => (sqlstate, message, detail, hint),
        other => panic!("expected diagnostic fields: {other:?}"),
    }
}

fn flags(engine: &Engine, column: &str) -> Vec<(String, bool)> {
    let mut rows = engine
        .sql(
            &format!(
                "SELECT conrelid::regclass::text AS conrelid, {column} AS flag FROM pg_constraint WHERE contype = 'f'"
            ),
            &[],
        )
        .unwrap()
        .rows
        .into_iter()
        .map(|row| {
            let Value::Str(table) = &row["conrelid"] else {
                panic!("unexpected conrelid");
            };
            let Value::Bool(flag) = row["flag"] else {
                panic!("unexpected {column}");
            };
            (table.clone(), flag)
        })
        .collect::<Vec<_>>();
    rows.sort();
    rows
}

#[test]
fn altering_a_partitioned_foreign_key_alters_its_copies() {
    let engine = Engine::new();
    for sql in [
        "CREATE TABLE pk (a integer PRIMARY KEY)",
        "CREATE TABLE fk (a integer REFERENCES pk, b integer) PARTITION BY RANGE (b)",
        "CREATE TABLE fk1 PARTITION OF fk FOR VALUES FROM (0) TO (10) PARTITION BY RANGE (b)",
        "CREATE TABLE fk11 PARTITION OF fk1 FOR VALUES FROM (0) TO (5)",
    ] {
        exec(&engine, sql);
    }
    assert_eq!(
        diagnostic(
            engine
                .sql(
                    "ALTER TABLE ONLY fk ALTER CONSTRAINT fk_a_fkey DEFERRABLE",
                    &[]
                )
                .unwrap_err()
        ),
        (
            "42P16".into(),
            "constraint must be altered in child tables too".into(),
            None,
            Some("Do not specify the ONLY keyword.".into())
        )
    );
    exec(
        &engine,
        "ALTER TABLE fk ALTER CONSTRAINT fk_a_fkey DEFERRABLE INITIALLY DEFERRED",
    );
    let all = |flag| {
        ["fk", "fk1", "fk11"]
            .into_iter()
            .map(|table| (table.to_string(), flag))
            .collect::<Vec<_>>()
    };
    assert_eq!(flags(&engine, "condeferred"), all(true));
    // A row routed to a leaf partition follows the copy's deferral.
    exec(&engine, "BEGIN");
    exec(&engine, "INSERT INTO fk VALUES (9, 1)");
    exec(&engine, "ROLLBACK");
    exec(
        &engine,
        "ALTER TABLE fk ALTER CONSTRAINT fk_a_fkey NOT ENFORCED",
    );
    assert_eq!(flags(&engine, "conenforced"), all(false));
    exec(&engine, "INSERT INTO fk VALUES (9, 1)");
    assert_violation(
        &engine,
        "ALTER TABLE fk ALTER CONSTRAINT fk_a_fkey ENFORCED",
        "insert or update on table \"fk11\" violates foreign key constraint \"fk_a_fkey\"",
    );
    exec(&engine, "DELETE FROM fk");
    exec(
        &engine,
        "ALTER TABLE fk ALTER CONSTRAINT fk_a_fkey ENFORCED",
    );
    assert_eq!(flags(&engine, "conenforced"), all(true));
    assert_eq!(flags(&engine, "convalidated"), all(true));
    assert_eq!(
        diagnostic(
            engine
                .sql(
                    "ALTER TABLE fk11 ALTER CONSTRAINT fk_a_fkey NOT DEFERRABLE",
                    &[]
                )
                .unwrap_err()
        ),
        (
            "55000".into(),
            "cannot alter constraint \"fk_a_fkey\" on relation \"fk11\"".into(),
            Some(
                "Constraint \"fk_a_fkey\" is derived from constraint \"fk_a_fkey\" of relation \"fk\"."
                    .into()
            ),
            Some("You may alter the constraint it derives from instead.".into())
        )
    );
    assert_error(
        &engine,
        "ALTER TABLE fk11 ALTER CONSTRAINT fk_a_fkey NO INHERIT",
        "42809",
        "constraint \"fk_a_fkey\" of relation \"fk11\" is not a not-null constraint",
    );
}
