//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `MERGE` statement: UPDATE / DELETE / INSERT branches based on
//! match condition.

use uqa_core::Value;
use uqa_engine::Engine;

fn setup() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE inventory (id INTEGER PRIMARY KEY, qty INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO inventory (id, qty) VALUES (1, 10), (2, 20)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE deltas (id INTEGER PRIMARY KEY, change INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO deltas (id, change) VALUES (1, 5), (3, 7)", &[])
        .unwrap();
    eng
}

#[test]
fn merge_updates_matched_inserts_unmatched() {
    let eng = setup();
    let r = eng
        .sql(
            "MERGE INTO inventory AS t USING deltas AS d ON t.id = d.id \
             WHEN MATCHED THEN UPDATE SET qty = qty + change \
             WHEN NOT MATCHED THEN INSERT (id, qty) VALUES (d.id, d.change)",
            &[],
        )
        .unwrap();
    assert_eq!(r.affected_rows, 2);
    let inv1 = eng
        .get_document("inventory", 1)
        .unwrap()
        .expect("inventory row 1");
    assert_eq!(inv1.get("qty"), Some(&Value::Int(15)));
    let inv2 = eng
        .get_document("inventory", 2)
        .unwrap()
        .expect("inventory row 2");
    assert_eq!(inv2.get("qty"), Some(&Value::Int(20)));
    let inv3 = eng
        .get_document("inventory", 3)
        .unwrap()
        .expect("inventory row 3");
    assert_eq!(inv3.get("qty"), Some(&Value::Int(7)));
}

#[test]
fn merge_returning_exposes_postgresql_18_row_images() {
    let eng = setup();
    let result = eng
        .sql(
            "MERGE INTO inventory AS t USING deltas AS d ON t.id = d.id \
             WHEN MATCHED THEN UPDATE SET qty = qty + change \
             WHEN NOT MATCHED THEN INSERT (id, qty) VALUES (d.id, d.change) \
             RETURNING merge_action() AS action, old.qty AS old_qty, new.qty AS new_qty",
            &[],
        )
        .unwrap();

    let update = result
        .rows
        .iter()
        .find(|row| row.get("action") == Some(&Value::Str("UPDATE".into())))
        .expect("MERGE UPDATE result");
    assert_eq!(update.get("old_qty"), Some(&Value::Int(10)));
    assert_eq!(update.get("new_qty"), Some(&Value::Int(15)));

    let insert = result
        .rows
        .iter()
        .find(|row| row.get("action") == Some(&Value::Str("INSERT".into())))
        .expect("MERGE INSERT result");
    assert_eq!(insert.get("old_qty"), Some(&Value::Null));
    assert_eq!(insert.get("new_qty"), Some(&Value::Int(7)));
}

#[test]
fn merge_when_matched_delete() {
    let eng = setup();
    let r = eng
        .sql(
            "MERGE INTO inventory AS t USING deltas AS d ON t.id = d.id \
             WHEN MATCHED THEN DELETE",
            &[],
        )
        .unwrap();
    assert_eq!(r.affected_rows, 1);
    assert!(eng.get_document("inventory", 1).unwrap().is_none());
    assert!(eng.get_document("inventory", 2).unwrap().is_some());
}

#[test]
fn merge_update_and_insert_enforce_composite_unique_keys() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE target (
            id INTEGER PRIMARY KEY,
            tenant TEXT,
            slug TEXT,
            UNIQUE (tenant, slug)
        )",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO target (id, tenant, slug) VALUES
            (1, 'a', 'one'), (2, 'a', 'two')",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE source (id INTEGER PRIMARY KEY, tenant TEXT, slug TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO source (id, tenant, slug) VALUES (2, 'a', 'one')",
        &[],
    )
    .unwrap();

    let update_error = eng
        .sql(
            "MERGE INTO target AS t USING source AS s ON t.id = s.id
             WHEN MATCHED THEN UPDATE SET tenant = s.tenant, slug = s.slug",
            &[],
        )
        .unwrap_err();
    assert!(update_error
        .to_string()
        .to_ascii_lowercase()
        .contains("unique"));

    eng.sql("DELETE FROM source", &[]).unwrap();
    eng.sql(
        "INSERT INTO source (id, tenant, slug) VALUES (3, 'a', 'one')",
        &[],
    )
    .unwrap();
    let insert_error = eng
        .sql(
            "MERGE INTO target AS t USING source AS s ON t.id = s.id
             WHEN NOT MATCHED THEN INSERT (id, tenant, slug)
             VALUES (s.id, s.tenant, s.slug)",
            &[],
        )
        .unwrap_err();
    assert!(insert_error
        .to_string()
        .to_ascii_lowercase()
        .contains("unique"));
}

#[test]
fn merge_insert_resolves_integer_primary_key_defaults_before_document_identity() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE target (
            id INTEGER PRIMARY KEY DEFAULT 11,
            payload TEXT
        )",
        &[],
    )
    .unwrap();
    eng.sql("CREATE TABLE source (payload TEXT)", &[]).unwrap();
    eng.sql("INSERT INTO source (payload) VALUES ('merged')", &[])
        .unwrap();

    eng.sql(
        "MERGE INTO target AS t USING source AS s ON FALSE
         WHEN NOT MATCHED THEN INSERT (payload) VALUES (s.payload)",
        &[],
    )
    .unwrap();

    assert_eq!(
        eng.get_document("target", 11)
            .unwrap()
            .expect("MERGE DEFAULT primary key must address the inserted document")["payload"],
        Value::Str("merged".into())
    );
}

#[test]
fn merge_rejects_unknown_target_columns_before_executing_a_branch() {
    let eng = setup();
    for sql in [
        "MERGE INTO inventory AS t USING deltas AS d ON t.id = d.id
         WHEN MATCHED THEN UPDATE SET misspelled = d.change",
        "MERGE INTO inventory AS t USING deltas AS d ON t.id = d.id
         WHEN NOT MATCHED THEN INSERT (id, misspelled) VALUES (d.id, d.change)",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert!(
            error
                .to_string()
                .to_ascii_lowercase()
                .contains("unknown column"),
            "unexpected error for {sql}: {error}"
        );
    }

    assert_eq!(
        eng.get_document("inventory", 1).unwrap().unwrap()["qty"],
        Value::Int(10)
    );
    assert!(eng.get_document("inventory", 3).unwrap().is_none());
}
