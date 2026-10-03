//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A foreign key that references a partitioned table derives a constraint on each partition of it, as `PostgreSQL`'s `addFkRecurseReferenced` and `CloneFkReferenced` create one: named from the constraint at the level the partition joins, following the partition tree, and visible in `pg_constraint` and the information schema.

use super::{exec, Engine, Value};

fn text(row: &uqa_sql::ResultRow, column: &str) -> String {
    match &row[column] {
        Value::Str(text) => text.clone(),
        Value::Null => String::new(),
        other => panic!("unexpected {column}: {other:?}"),
    }
}

/// The foreign key rows of `table`: name, referenced relation and the name of the constraint each derives from.
fn derived(engine: &Engine, table: &str) -> Vec<(String, String, String)> {
    let mut rows = engine
        .sql(
            &format!(
                "SELECT conname, confrelid::regclass::text AS frel, (SELECT p.conname FROM pg_constraint p WHERE p.oid = c.conparentid) AS parent FROM pg_constraint c WHERE contype = 'f' AND conrelid = '{table}'::regclass"
            ),
            &[],
        )
        .unwrap()
        .rows
        .iter()
        .map(|row| (text(row, "conname"), text(row, "frel"), text(row, "parent")))
        .collect::<Vec<_>>();
    rows.sort();
    rows
}

fn triples(rows: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    rows.iter()
        .map(|(name, referenced, parent)| {
            (
                (*name).to_string(),
                (*referenced).to_string(),
                (*parent).to_string(),
            )
        })
        .collect()
}

fn validated(engine: &Engine) -> Vec<(String, bool)> {
    let mut rows = engine
        .sql(
            "SELECT conname, convalidated FROM pg_constraint WHERE contype = 'f' AND conrelid = 'fk'::regclass",
            &[],
        )
        .unwrap()
        .rows
        .iter()
        .map(|row| {
            let Value::Bool(validated) = row["convalidated"] else {
                panic!("unexpected convalidated");
            };
            (text(row, "conname"), validated)
        })
        .collect::<Vec<_>>();
    rows.sort();
    rows
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

fn referenced_tree(engine: &Engine) {
    for sql in [
        "CREATE TABLE pk (a integer PRIMARY KEY) PARTITION BY RANGE (a)",
        "CREATE TABLE pk1 PARTITION OF pk FOR VALUES FROM (0) TO (10)",
        "CREATE TABLE pk2 PARTITION OF pk FOR VALUES FROM (10) TO (20) PARTITION BY RANGE (a)",
        "CREATE TABLE pk21 PARTITION OF pk2 FOR VALUES FROM (10) TO (15)",
    ] {
        exec(engine, sql);
    }
}

fn referencing_tree(engine: &Engine) {
    referenced_tree(engine);
    for sql in [
        "CREATE TABLE fk (a integer, b integer) PARTITION BY RANGE (b)",
        "CREATE TABLE fk1 PARTITION OF fk FOR VALUES FROM (0) TO (10)",
        "CREATE TABLE fk2 PARTITION OF fk FOR VALUES FROM (10) TO (20)",
        "ALTER TABLE fk ADD FOREIGN KEY (a) REFERENCES pk NOT VALID",
    ] {
        exec(engine, sql);
    }
}

#[test]
fn a_foreign_key_derives_a_constraint_on_each_referenced_partition() {
    let engine = Engine::new();
    referencing_tree(&engine);
    assert_eq!(
        derived(&engine, "fk"),
        triples(&[
            ("fk_a_fkey", "pk", ""),
            ("fk_a_fkey_1", "pk1", "fk_a_fkey"),
            ("fk_a_fkey_2", "pk2", "fk_a_fkey"),
            ("fk_a_fkey_3", "pk21", "fk_a_fkey_2"),
        ])
    );
    // The partitions' copies of the foreign key derive none.
    assert_eq!(derived(&engine, "fk1"), triples(&[("fk_a_fkey", "pk", "")]));
    let row = &engine
        .sql(
            "SELECT conislocal, coninhcount, connoinherit, conindid::regclass::text AS idx FROM pg_constraint WHERE conname = 'fk_a_fkey_3'",
            &[],
        )
        .unwrap()
        .rows[0];
    assert_eq!(row["conislocal"], Value::Bool(false));
    assert_eq!(row["coninhcount"], Value::Int(1));
    assert_eq!(row["connoinherit"], Value::Bool(false));
    assert_eq!(text(row, "idx"), "pk21_pkey");
    // Validating a derived constraint validates it alone; validating the foreign key validates all.
    exec(&engine, "ALTER TABLE fk VALIDATE CONSTRAINT fk_a_fkey_1");
    assert_eq!(
        validated(&engine),
        [
            ("fk_a_fkey".to_string(), false),
            ("fk_a_fkey_1".to_string(), true),
            ("fk_a_fkey_2".to_string(), false),
            ("fk_a_fkey_3".to_string(), false),
        ]
    );
    exec(&engine, "ALTER TABLE fk VALIDATE CONSTRAINT fk_a_fkey");
    assert!(validated(&engine).iter().all(|(_, validated)| *validated));
    let names = engine
        .sql(
            "SELECT constraint_name FROM information_schema.table_constraints WHERE table_name = 'fk' AND constraint_type = 'FOREIGN KEY' ORDER BY 1",
            &[],
        )
        .unwrap()
        .rows
        .iter()
        .map(|row| text(row, "constraint_name"))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["fk_a_fkey", "fk_a_fkey_1", "fk_a_fkey_2", "fk_a_fkey_3"]
    );
}

#[test]
fn partitions_that_join_later_derive_constraints_named_from_their_parent_level() {
    let engine = Engine::new();
    referencing_tree(&engine);
    for sql in [
        "CREATE TABLE pk22 PARTITION OF pk2 FOR VALUES FROM (15) TO (20)",
        "CREATE TABLE pk3 (a integer PRIMARY KEY)",
        "ALTER TABLE pk ATTACH PARTITION pk3 FOR VALUES FROM (20) TO (30)",
    ] {
        exec(&engine, sql);
    }
    assert_eq!(
        derived(&engine, "fk"),
        triples(&[
            ("fk_a_fkey", "pk", ""),
            ("fk_a_fkey_1", "pk1", "fk_a_fkey"),
            ("fk_a_fkey_2", "pk2", "fk_a_fkey"),
            ("fk_a_fkey_2_1", "pk22", "fk_a_fkey_2"),
            ("fk_a_fkey_3", "pk21", "fk_a_fkey_2"),
            ("fk_a_fkey_4", "pk3", "fk_a_fkey"),
        ])
    );
    exec(&engine, "ALTER TABLE pk DETACH PARTITION pk3");
    assert!(!derived(&engine, "fk")
        .iter()
        .any(|(name, _, _)| name == "fk_a_fkey_4"));
}

#[test]
fn derived_constraints_refuse_alteration_and_removal_but_rename() {
    let engine = Engine::new();
    referencing_tree(&engine);
    assert_eq!(
        diagnostic(
            engine
                .sql(
                    "ALTER TABLE fk ALTER CONSTRAINT fk_a_fkey_1 NOT DEFERRABLE",
                    &[]
                )
                .unwrap_err()
        ),
        (
            "55000".into(),
            "cannot alter constraint \"fk_a_fkey_1\" on relation \"fk\"".into(),
            Some(
                "Constraint \"fk_a_fkey_1\" is derived from constraint \"fk_a_fkey\" of relation \"fk\"."
                    .into()
            ),
            Some("You may alter the constraint it derives from instead.".into())
        )
    );
    let error = engine
        .sql("ALTER TABLE fk DROP CONSTRAINT fk_a_fkey_1", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P16"), "{error}");
    assert!(error
        .to_string()
        .contains("cannot drop inherited constraint \"fk_a_fkey_1\" of relation \"fk\""));
    let error = engine
        .sql(
            "ALTER TABLE fk ADD CONSTRAINT fk_a_fkey_2 CHECK (a > 0)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42710"), "{error}");
    assert!(error
        .to_string()
        .contains("constraint \"fk_a_fkey_2\" for relation \"fk\" already exists"));
    exec(
        &engine,
        "ALTER TABLE fk RENAME CONSTRAINT fk_a_fkey_1 TO renamed_derived",
    );
    assert!(derived(&engine, "fk").contains(&(
        "renamed_derived".to_string(),
        "pk1".to_string(),
        "fk_a_fkey".to_string()
    )));
}

#[test]
fn a_detached_referencing_partition_derives_constraints_and_an_attached_one_drops_them() {
    let engine = Engine::new();
    referencing_tree(&engine);
    // The name a rename frees is chosen again.
    exec(
        &engine,
        "ALTER TABLE fk RENAME CONSTRAINT fk_a_fkey_1 TO renamed_derived",
    );
    exec(&engine, "ALTER TABLE fk DETACH PARTITION fk1");
    assert_eq!(
        derived(&engine, "fk1"),
        triples(&[
            ("fk_a_fkey", "pk", ""),
            ("fk_a_fkey_1", "pk1", "fk_a_fkey"),
            ("fk_a_fkey_4", "pk2", "fk_a_fkey"),
            ("fk_a_fkey_5", "pk21", "fk_a_fkey_4"),
        ])
    );
    exec(
        &engine,
        "ALTER TABLE fk ATTACH PARTITION fk1 FOR VALUES FROM (0) TO (10)",
    );
    assert_eq!(derived(&engine, "fk1"), triples(&[("fk_a_fkey", "pk", "")]));
}

#[test]
fn validating_a_derived_constraint_of_a_plain_referencing_table_reads_its_partition() {
    let engine = Engine::new();
    referenced_tree(&engine);
    for sql in [
        "INSERT INTO pk VALUES (1), (12)",
        "CREATE TABLE fk (a integer)",
        "INSERT INTO fk VALUES (1), (12)",
        "ALTER TABLE fk ADD FOREIGN KEY (a) REFERENCES pk NOT VALID",
    ] {
        exec(&engine, sql);
    }
    // As PostgreSQL does, the referencing rows are validated against the derived constraint's partition alone.
    let error = engine
        .sql("ALTER TABLE fk VALIDATE CONSTRAINT fk_a_fkey_1", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23503"), "{error}");
    assert!(
        error.to_string().contains(
            "insert or update on table \"fk\" violates foreign key constraint \"fk_a_fkey_1\""
        ),
        "{error}"
    );
    exec(&engine, "ALTER TABLE fk VALIDATE CONSTRAINT fk_a_fkey");
    assert!(validated(&engine).iter().all(|(_, validated)| *validated));
}

#[test]
fn reopening_derives_the_constraints_that_earlier_releases_did_not_record() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("derived-constraint-repair.db");
    {
        let engine = crate::native_storage::legacy_engine(&path);
        referenced_tree(&engine);
        exec(&engine, "CREATE TABLE fk (a integer REFERENCES pk)");
    }
    let connection = rusqlite::Connection::open(&path).unwrap();
    let columns: String = connection
        .query_row(
            "SELECT columns FROM _tables WHERE relation_name = 'fk'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut columns: Vec<uqa_sql::ast::ColumnDef> = serde_json::from_str(&columns).unwrap();
    let reference = columns[0].references.as_mut().unwrap();
    assert_eq!(reference.referenced_partitions.len(), 3);
    reference.referenced_partitions.clear();
    connection
        .execute(
            "UPDATE _tables SET columns = ?1 WHERE relation_name = 'fk'",
            rusqlite::params![serde_json::to_string(&columns).unwrap()],
        )
        .unwrap();
    drop(connection);
    let reopened = Engine::open(&path).unwrap();
    assert_eq!(
        derived(&reopened, "fk"),
        triples(&[
            ("fk_a_fkey", "pk", ""),
            ("fk_a_fkey_1", "pk1", "fk_a_fkey"),
            ("fk_a_fkey_2", "pk2", "fk_a_fkey"),
            ("fk_a_fkey_3", "pk21", "fk_a_fkey_2"),
        ])
    );
}
