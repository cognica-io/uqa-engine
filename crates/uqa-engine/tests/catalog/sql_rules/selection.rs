//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18.4 reference: ordered ALSO actions retain every original matching ordinal.

use super::*;

#[test]
fn rule_selection_preserves_sparse_actions_and_conditional_suppression() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE selection_items(id INTEGER PRIMARY KEY, value INTEGER);
         CREATE TABLE selection_log(seq BIGSERIAL PRIMARY KEY, message TEXT);
         INSERT INTO selection_items SELECT n,10 FROM generate_series(1,8) AS s(n);
         CREATE RULE z_suppress_even AS ON UPDATE TO selection_items
           WHERE NEW.id % 2 = 0 DO INSTEAD NOTHING;
         CREATE RULE b_log_all AS ON UPDATE TO selection_items
           DO ALSO INSERT INTO selection_log(message) SELECT 'b:' || NEW.id;
         CREATE RULE a_log_thirds AS ON UPDATE TO selection_items
           WHERE NEW.id % 3 = 0 DO ALSO INSERT INTO selection_log(message) VALUES ('a:' || NEW.id);
         CREATE RULE c_never AS ON UPDATE TO selection_items
           WHERE NEW.id < 0 DO ALSO INSERT INTO selection_log(message) VALUES ('unexpected')",
    );
    engine.set_query_memory_limit(Some(16 * 1024));
    exec(&engine, "UPDATE selection_items SET value=id WHERE id<=6");
    let rows = exec(&engine, "SELECT id,value FROM selection_items ORDER BY id");
    for (index, expected) in [1, 10, 3, 10, 5, 10, 10, 10].into_iter().enumerate() {
        assert_eq!(rows.value_at(index, 0), Some(&Value::Int(index as i64 + 1)));
        assert_eq!(rows.value_at(index, 1), Some(&Value::Int(expected)));
    }
    assert_eq!(rows.rows.len(), 8);
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM selection_log ORDER BY seq",
            "message"
        ),
        ["a:3", "a:6", "b:1", "b:2", "b:3", "b:4", "b:5", "b:6"]
    );
    exec(&engine, "UPDATE selection_items SET value=id WHERE id<0");
    assert_eq!(
        exec(&engine, "SELECT count(*) FROM selection_log").value_at(0, 0),
        Some(&Value::Int(8))
    );
}
