//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public rewrite-rule candidate lifetime under the query memory allowance.

use super::*;

#[test]
fn suppressed_rule_deletes_do_not_retain_complete_candidate_documents() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE suppressed_deletes(id INTEGER PRIMARY KEY, body TEXT);
        INSERT INTO suppressed_deletes SELECT n, repeat('x',32768) FROM generate_series(1,512) AS s(n);
        CREATE RULE suppress_delete AS ON DELETE TO suppressed_deletes DO INSTEAD NOTHING");
    engine.set_query_memory_limit(Some(64 * 1024));
    for sql in [
        "DELETE FROM suppressed_deletes",
        "DELETE FROM suppressed_deletes AS e USING (VALUES (1)) AS s(n) WHERE e.id >= s.n",
    ] {
        let allocated = allocation_counter::measure(|| {
            exec(&engine, sql);
        });
        assert!(
            allocated.bytes_max < 2 * 1024 * 1024,
            "{sql}: {allocated:?}"
        );
    }
    let unchanged = exec(
        &engine,
        "SELECT count(*), min(length(body)), max(length(body)) FROM suppressed_deletes",
    );
    assert_eq!(unchanged.value_at(0, 0), Some(&Value::Int(512)));
    assert_eq!(unchanged.value_at(0, 1), Some(&Value::Int(32768)));
    assert_eq!(unchanged.value_at(0, 2), Some(&Value::Int(32768)));
}

#[test]
fn suppressed_rule_updates_do_not_retain_complete_candidate_documents() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE suppressed_events(id INTEGER PRIMARY KEY, body TEXT);
        INSERT INTO suppressed_events SELECT n, repeat('x',32768) FROM generate_series(1,512) AS s(n);
        CREATE RULE suppress_update AS ON UPDATE TO suppressed_events DO INSTEAD NOTHING;
        CREATE VIEW suppressed_view AS SELECT id, body FROM suppressed_events;
        CREATE RULE suppress_view_update AS ON UPDATE TO suppressed_view DO INSTEAD NOTHING");
    engine.set_query_memory_limit(Some(64 * 1024));
    for sql in [
        "UPDATE suppressed_events SET body=body || 'changed'",
        "UPDATE suppressed_events AS e SET body=e.body || s.suffix FROM (VALUES ('changed')) AS s(suffix)",
        "UPDATE suppressed_view SET body=body || 'changed'",
    ] {
        // Includes analysis and every candidate image; existing table storage is outside this query's allocation interval.
        let allocated = allocation_counter::measure(|| {
            exec(&engine, sql);
        });
        assert!(allocated.bytes_max < 2 * 1024 * 1024, "{sql}: {allocated:?}");
    }
    let unchanged = exec(
        &engine,
        "SELECT count(*), min(length(body)), max(length(body)) FROM suppressed_events",
    );
    assert_eq!(unchanged.value_at(0, 0), Some(&Value::Int(512)));
    assert_eq!(unchanged.value_at(0, 1), Some(&Value::Int(32768)));
    assert_eq!(unchanged.value_at(0, 2), Some(&Value::Int(32768)));
}

#[test]
fn suppressed_view_insert_reuses_bounded_rule_images_for_condition_and_action_inputs() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE lazy_input_base(id INTEGER, body TEXT);
         CREATE VIEW lazy_input_view AS SELECT id,body FROM lazy_input_base;
         CREATE TABLE lazy_input_log(kind INTEGER, id INTEGER, bytes INTEGER);
         CREATE RULE a_measure AS ON INSERT TO lazy_input_view
           WHERE length(NEW.body)=32768 DO ALSO
           INSERT INTO lazy_input_log VALUES (1,NEW.id,length(NEW.body));
         CREATE RULE b_reuse AS ON INSERT TO lazy_input_view
           WHERE length(NEW.body)>0 DO ALSO
           INSERT INTO lazy_input_log VALUES (2,NEW.id,length(NEW.body));
         CREATE RULE z_suppress AS ON INSERT TO lazy_input_view DO INSTEAD NOTHING",
    );
    engine.set_query_memory_limit(Some(64 * 1024));
    let values = (1..=512)
        .map(|id| format!("({id},repeat('x',32768))"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("INSERT INTO lazy_input_view VALUES {values}");
    let allocated = allocation_counter::measure(|| {
        assert_eq!(exec(&engine, &sql).affected_rows, 0);
    });
    assert!(allocated.bytes_max < 8 * 1024 * 1024, "{allocated:?}");
    assert_eq!(
        exec(&engine, "SELECT count(*) FROM lazy_input_base").value_at(0, 0),
        Some(&Value::Int(0))
    );
    let rows = exec(
        &engine,
        "SELECT kind,count(*),sum(id),min(bytes),max(bytes) FROM lazy_input_log GROUP BY kind ORDER BY kind",
    );
    assert_eq!(rows.rows.len(), 2);
    for row in 0..2 {
        for (column, expected) in [row as i64 + 1, 512, 131_328, 32768, 32768]
            .into_iter()
            .enumerate()
        {
            assert_eq!(rows.value_at(row, column), Some(&Value::Int(expected)));
        }
    }
}

#[test]
fn suppressed_view_update_delete_bound_original_rows_and_pending_rule_images() {
    let engine = Engine::new();
    exec(&engine,
        "CREATE TABLE view_pending_base(id INTEGER PRIMARY KEY, body TEXT);\
         INSERT INTO view_pending_base SELECT n,repeat('x',32768) FROM generate_series(1,512) AS s(n);\
         CREATE VIEW view_pending_input AS SELECT id,body FROM view_pending_base UNION ALL SELECT id,body FROM view_pending_base WHERE false;\
         CREATE TABLE view_pending_log(kind INTEGER,id INTEGER,old_bytes INTEGER,new_bytes INTEGER);\
         CREATE RULE a_update AS ON UPDATE TO view_pending_input DO ALSO INSERT INTO view_pending_log VALUES(1,OLD.id,length(OLD.body),length(NEW.body));\
         CREATE RULE z_update AS ON UPDATE TO view_pending_input DO INSTEAD NOTHING;\
         CREATE RULE a_delete AS ON DELETE TO view_pending_input DO ALSO INSERT INTO view_pending_log VALUES(2,OLD.id,length(OLD.body),NULL);\
         CREATE RULE z_delete AS ON DELETE TO view_pending_input DO INSTEAD NOTHING;\
         SET work_mem='64kB'",
    );
    let mut peaks = Vec::new();
    for sql in [
        "UPDATE view_pending_input SET body=repeat('y',32768)",
        "DELETE FROM view_pending_input",
    ] {
        let allocated = allocation_counter::measure(|| {
            assert_eq!(exec(&engine, sql).affected_rows, 0);
        });
        peaks.push((sql, allocated.bytes_max));
    }
    let output = exec(&engine, "SELECT kind,count(*),sum(id),min(old_bytes),max(old_bytes),min(new_bytes),max(new_bytes) FROM view_pending_log GROUP BY kind ORDER BY kind");
    assert_eq!(output.rows.len(), 2);
    for (row, values) in [
        vec![
            Value::Int(1),
            Value::Int(512),
            Value::Int(131_328),
            Value::Int(32768),
            Value::Int(32768),
            Value::Int(32768),
            Value::Int(32768),
        ],
        vec![
            Value::Int(2),
            Value::Int(512),
            Value::Int(131_328),
            Value::Int(32768),
            Value::Int(32768),
            Value::Null,
            Value::Null,
        ],
    ]
    .into_iter()
    .enumerate()
    {
        for (column, value) in values.iter().enumerate() {
            assert_eq!(output.value_at(row, column), Some(value));
        }
    }
    let base=exec(&engine, "SELECT count(*),min(length(body)),max(length(body)),min(substr(body,1,1)) FROM view_pending_base");
    for (column, value) in [
        Value::Int(512),
        Value::Int(32768),
        Value::Int(32768),
        Value::Str("x".into()),
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(base.value_at(0, column), Some(value));
    }
    assert!(
        peaks.iter().all(|(_, bytes)| *bytes < 8 * 1024 * 1024),
        "view command peak allocations: {peaks:?}"
    );
}
