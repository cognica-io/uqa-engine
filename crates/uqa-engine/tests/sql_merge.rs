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
    let inv1 = eng.get_document("inventory", 1).unwrap();
    assert_eq!(inv1.get("qty"), Some(&Value::Int(15)));
    let inv2 = eng.get_document("inventory", 2).unwrap();
    assert_eq!(inv2.get("qty"), Some(&Value::Int(20)));
    let inv3 = eng.get_document("inventory", 3).unwrap();
    assert_eq!(inv3.get("qty"), Some(&Value::Int(7)));
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
    assert!(eng.get_document("inventory", 1).is_none());
    assert!(eng.get_document("inventory", 2).is_some());
}
