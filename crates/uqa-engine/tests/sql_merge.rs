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

#[test]
fn merge_not_matched_by_source_orders_actions_and_returns_all_row_images() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE target (id INTEGER PRIMARY KEY, val INTEGER, note TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO target VALUES (1, 10, 'matched'), (2, 20, 'keep'), (3, 30, 'remove')",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE source (id INTEGER PRIMARY KEY, delta INTEGER, marker TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO source VALUES (1, 5, 'm'), (4, 40, 'new')", &[])
        .unwrap();

    let result = eng
        .sql(
            "MERGE INTO target AS t USING source AS s ON t.id = s.id
             WHEN NOT MATCHED BY SOURCE AND t.id = 2 THEN UPDATE SET val = t.val + 1, note = 'source-missing-updated'
             WHEN MATCHED THEN UPDATE SET val = t.val + s.delta, note = s.marker
             WHEN NOT MATCHED BY TARGET THEN INSERT (id, val, note) VALUES (s.id, s.delta, s.marker)
             WHEN NOT MATCHED BY SOURCE THEN DELETE
             RETURNING WITH (OLD AS before, NEW AS after)
             merge_action() AS action, s.id AS source_id,
             before.id AS old_id, before.val AS old_val, before.note AS old_note,
             after.id AS new_id, after.val AS new_val, after.note AS new_note,
             t.id AS target_id, t.val AS target_val",
            &[],
        )
        .unwrap();

    assert_eq!(result.affected_rows, 4);
    let row = |action: &str, id: i64| {
        result
            .rows
            .iter()
            .find(|row| {
                row.get("action") == Some(&Value::Str(action.into()))
                    && (row.get("old_id") == Some(&Value::Int(id))
                        || row.get("new_id") == Some(&Value::Int(id)))
            })
            .unwrap_or_else(|| panic!("missing {action} row for id {id}: {result:?}"))
    };
    let matched = row("UPDATE", 1);
    assert_eq!(matched.get("source_id"), Some(&Value::Int(1)));
    assert_eq!(matched.get("old_val"), Some(&Value::Int(10)));
    assert_eq!(matched.get("new_val"), Some(&Value::Int(15)));
    let source_missing_update = row("UPDATE", 2);
    assert_eq!(source_missing_update.get("source_id"), Some(&Value::Null));
    assert_eq!(source_missing_update.get("old_val"), Some(&Value::Int(20)));
    assert_eq!(source_missing_update.get("new_val"), Some(&Value::Int(21)));
    let source_missing_delete = row("DELETE", 3);
    assert_eq!(source_missing_delete.get("source_id"), Some(&Value::Null));
    assert_eq!(source_missing_delete.get("old_val"), Some(&Value::Int(30)));
    assert_eq!(source_missing_delete.get("new_id"), Some(&Value::Null));
    assert_eq!(source_missing_delete.get("target_id"), Some(&Value::Int(3)));
    let inserted = row("INSERT", 4);
    assert_eq!(inserted.get("source_id"), Some(&Value::Int(4)));
    assert_eq!(inserted.get("old_id"), Some(&Value::Null));
    assert_eq!(inserted.get("new_val"), Some(&Value::Int(40)));
}

#[test]
fn merge_do_nothing_actions_return_no_rows_and_do_not_count() {
    let eng = setup();
    let result = eng
        .sql(
            "MERGE INTO inventory AS t USING deltas AS d ON t.id = d.id
             WHEN MATCHED THEN DO NOTHING
             WHEN NOT MATCHED BY SOURCE THEN DO NOTHING
             WHEN NOT MATCHED BY TARGET THEN DO NOTHING
             RETURNING WITH (OLD AS before, NEW AS after)
             merge_action() AS action, d.id AS source_id, before.id AS old_id, after.id AS new_id",
            &[],
        )
        .unwrap();
    assert_eq!(result.affected_rows, 0);
    assert!(result.rows.is_empty());
    assert_eq!(
        eng.sql("SELECT COUNT(*) AS n FROM inventory", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(2)
    );
}

#[test]
fn merge_candidate_visibility_is_validated_before_any_candidate_matches() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE target (id INTEGER PRIMARY KEY, val INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE source (id INTEGER PRIMARY KEY, val INTEGER)",
        &[],
    )
    .unwrap();

    for sql in [
        "MERGE INTO target t USING source s ON t.id = s.id WHEN NOT MATCHED BY SOURCE AND s.id IS NULL THEN DELETE",
        "MERGE INTO target t USING source s ON t.id = s.id WHEN NOT MATCHED BY SOURCE THEN UPDATE SET val = s.val",
        "MERGE INTO target t USING source s ON t.id = s.id WHEN NOT MATCHED BY TARGET AND t.id IS NULL THEN DO NOTHING",
        "MERGE INTO target t USING source s ON t.id = s.id WHEN NOT MATCHED BY TARGET THEN INSERT (id, val) VALUES (t.id, t.val)",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P01"), "{sql}: {error}");
    }

    for sql in [
        "MERGE INTO target t USING source s ON 1 WHEN MATCHED THEN DO NOTHING",
        "MERGE INTO target t USING source s ON t.id = s.id WHEN NOT MATCHED BY SOURCE AND 1 THEN DELETE",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42804"), "{sql}: {error}");
    }

    let full_join_error = eng
        .sql(
            "MERGE INTO target t USING source s ON t.id > s.id
             WHEN NOT MATCHED BY SOURCE THEN DELETE
             WHEN NOT MATCHED BY TARGET THEN INSERT (id, val) VALUES (s.id, s.val)",
            &[],
        )
        .unwrap_err();
    assert_eq!(full_join_error.sqlstate(), Some("0A000"));
    assert!(full_join_error.to_string().contains("FULL JOIN"));
}

#[test]
fn merge_unqualified_columns_resolve_in_the_candidate_kind_scope() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE target (id INTEGER PRIMARY KEY, val INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO target VALUES (1, 10), (2, 20)", &[])
        .unwrap();
    eng.sql(
        "CREATE TABLE source (id INTEGER PRIMARY KEY, val INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO source VALUES (1, 5), (3, 30)", &[])
        .unwrap();

    eng.sql(
        "MERGE INTO target t USING source s ON t.id = s.id
         WHEN MATCHED THEN DO NOTHING
         WHEN NOT MATCHED BY SOURCE AND id = 2 THEN UPDATE SET val = val + 1
         WHEN NOT MATCHED BY TARGET AND id = 3 THEN INSERT (id, val) VALUES (id, val)",
        &[],
    )
    .unwrap();

    let result = eng
        .sql("SELECT id, val FROM target ORDER BY id", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 3);
    assert_eq!(result.rows[1]["val"], Value::Int(21));
    assert_eq!(result.rows[2]["id"], Value::Int(3));
    assert_eq!(result.rows[2]["val"], Value::Int(30));
}

#[test]
fn merge_full_join_allows_one_source_row_to_change_multiple_targets() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE target (id INTEGER PRIMARY KEY, group_id INTEGER, val INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO target VALUES (1, 7, 10), (2, 7, 20)", &[])
        .unwrap();
    eng.sql(
        "CREATE TABLE source (group_id INTEGER PRIMARY KEY, delta INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO source VALUES (7, 5)", &[]).unwrap();

    let result = eng
        .sql(
            "MERGE INTO target t USING source s ON t.group_id = s.group_id
             WHEN MATCHED THEN UPDATE SET val = t.val + s.delta
             RETURNING t.id AS id, old.val AS old_val, new.val AS new_val",
            &[],
        )
        .unwrap();
    assert_eq!(result.affected_rows, 2);
    assert!(result.rows.iter().any(|row| row["id"] == Value::Int(1)
        && row["old_val"] == Value::Int(10)
        && row["new_val"] == Value::Int(15)));
    assert!(result.rows.iter().any(|row| row["id"] == Value::Int(2)
        && row["old_val"] == Value::Int(20)
        && row["new_val"] == Value::Int(25)));
}

#[test]
fn merge_rejects_two_mutations_of_one_target_atomically() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE target (id INTEGER PRIMARY KEY, val INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO target VALUES (1, 10)", &[]).unwrap();
    eng.sql(
        "CREATE TABLE source (sequence INTEGER PRIMARY KEY, id INTEGER, delta INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO source VALUES (1, 1, 2), (2, 1, 3)", &[])
        .unwrap();

    let error = eng
        .sql(
            "MERGE INTO target t USING source s ON t.id = s.id
             WHEN MATCHED THEN UPDATE SET val = t.val + s.delta",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("21000"));
    assert!(error
        .to_string()
        .contains("cannot affect row a second time"));
    assert_eq!(
        eng.sql("SELECT val FROM target", &[]).unwrap().rows[0]["val"],
        Value::Int(10)
    );
}

#[test]
fn merge_returning_star_orders_source_columns_before_target_columns() {
    let eng = setup();
    let result = eng
        .sql(
            "MERGE INTO inventory AS t USING deltas AS d ON t.id = d.id
             WHEN MATCHED THEN UPDATE SET qty = t.qty + d.change
             WHEN NOT MATCHED BY TARGET THEN INSERT (id, qty) VALUES (d.id, d.change)
             RETURNING *",
            &[],
        )
        .unwrap();

    assert_eq!(result.columns, vec!["id", "change", "id", "qty"]);
    let positions = result
        .positional_rows
        .as_ref()
        .expect("duplicate output labels retain positions");
    assert!(positions
        .iter()
        .any(|row| row == &vec![Value::Int(1), Value::Int(5), Value::Int(1), Value::Int(15)]));
    assert!(positions
        .iter()
        .any(|row| row == &vec![Value::Int(3), Value::Int(7), Value::Int(3), Value::Int(7)]));

    let qualified = setup();
    let qualified_result = qualified
        .sql(
            "MERGE INTO inventory AS t USING deltas AS d ON t.id = d.id
             WHEN MATCHED THEN UPDATE SET qty = t.qty + d.change
             WHEN NOT MATCHED BY TARGET THEN INSERT (id, qty) VALUES (d.id, d.change)
             RETURNING WITH (OLD AS before, NEW AS after) d.*, before.*, after.*",
            &[],
        )
        .unwrap();
    assert_eq!(
        qualified_result.columns,
        vec!["id", "change", "id", "qty", "id", "qty"]
    );
    let qualified_positions = qualified_result
        .positional_rows
        .as_ref()
        .expect("qualified row-image stars retain duplicate positions");
    assert!(qualified_positions.iter().any(|row| row
        == &vec![
            Value::Int(1),
            Value::Int(5),
            Value::Int(1),
            Value::Int(10),
            Value::Int(1),
            Value::Int(15),
        ]));
    assert!(qualified_positions.iter().any(|row| row
        == &vec![
            Value::Int(3),
            Value::Int(7),
            Value::Null,
            Value::Null,
            Value::Int(3),
            Value::Int(7),
        ]));
}
