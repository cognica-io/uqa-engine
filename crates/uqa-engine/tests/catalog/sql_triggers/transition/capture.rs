//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18.4 transition images across repeated trigger reads, rollback and reopen.

use super::{exec, strings, Engine, TempDir, Value};

fn open(provider: usize, directory: &TempDir) -> Engine {
    let path = directory.path().join("transition-capture.db");
    match provider {
        0 => Engine::new(),
        1 => Engine::open(&path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn captured_transition_images_preserve_repeated_reads_empty_events_and_rollback(
    #[case] provider: usize,
) {
    let directory = TempDir::new().unwrap();
    let engine = open(provider, &directory);
    exec(
        &engine,
        "CREATE TABLE items(id integer PRIMARY KEY, value integer, body text);
        CREATE TABLE audit(seq bigserial PRIMARY KEY, event text, old_rows text, new_rows text);
        CREATE FUNCTION capture() RETURNS trigger LANGUAGE plpgsql AS $$
        DECLARE old_images text; new_images text;
        BEGIN
         IF TG_OP IN ('UPDATE','DELETE') THEN
          SELECT string_agg(id::text || ':' || value::text || ':' || length(body)::text, ',' ORDER BY id) INTO old_images FROM old_rows;
         END IF;
         IF TG_OP IN ('INSERT','UPDATE') THEN
          SELECT string_agg(id::text || ':' || value::text || ':' || length(body)::text, ',' ORDER BY id) INTO new_images FROM new_rows;
         END IF;
         INSERT INTO audit(event,old_rows,new_rows) VALUES (TG_OP || ':' || TG_LEVEL,old_images,new_images);
         RETURN NULL;
        END $$;
        CREATE TRIGGER after_insert AFTER INSERT ON items REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION capture();
        CREATE TRIGGER after_update_row AFTER UPDATE ON items REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH ROW EXECUTE FUNCTION capture();
        CREATE TRIGGER after_update_statement AFTER UPDATE ON items REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION capture();
        CREATE TRIGGER after_delete AFTER DELETE ON items REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION capture()",
    );
    engine.set_query_memory_limit(Some(64 * 1024));
    for sql in [
        "INSERT INTO items VALUES (3,30,repeat('x',32768)),(1,10,repeat('y',32768)),(2,20,repeat('z',32768))",
        "UPDATE items SET value=value+1, body=body||'n'",
        "UPDATE items SET value=value+1 WHERE false",
        "DELETE FROM items WHERE id<>2",
    ] {
        exec(&engine, sql);
    }
    let expected = [
        "INSERT:STATEMENT|empty|1:10:32768,2:20:32768,3:30:32768",
        "UPDATE:ROW|1:10:32768,2:20:32768,3:30:32768|1:11:32769,2:21:32769,3:31:32769",
        "UPDATE:ROW|1:10:32768,2:20:32768,3:30:32768|1:11:32769,2:21:32769,3:31:32769",
        "UPDATE:ROW|1:10:32768,2:20:32768,3:30:32768|1:11:32769,2:21:32769,3:31:32769",
        "UPDATE:STATEMENT|1:10:32768,2:20:32768,3:30:32768|1:11:32769,2:21:32769,3:31:32769",
        "UPDATE:STATEMENT|empty|empty",
        "DELETE:STATEMENT|1:11:32769,3:31:32769|empty",
    ];
    let audit = "SELECT event||'|'||coalesce(old_rows,'empty')||'|'||coalesce(new_rows,'empty') AS message FROM audit ORDER BY seq";
    assert_eq!(strings(&engine, audit, "message"), expected);
    exec(&engine, "BEGIN");
    exec(&engine, "UPDATE items SET value=90");
    exec(&engine, "ROLLBACK");
    assert_eq!(strings(&engine, audit, "message"), expected);
    let row = exec(&engine, "SELECT id,value,length(body) FROM items");
    for (column, value) in [2, 21, 32769].into_iter().enumerate() {
        assert_eq!(row.value_at(0, column), Some(&Value::Int(value)));
    }
    assert_eq!(row.rows.len(), 1);
    drop(engine);
    if provider != 0 {
        let engine = open(provider, &directory);
        assert_eq!(strings(&engine, audit, "message"), expected);
        assert_eq!(
            exec(&engine, "SELECT value FROM items").value_at(0, 0),
            Some(&Value::Int(21))
        );
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn spilled_after_events_preserve_cascade_row_images_statement_replacement_and_reopen(
    #[case] provider: usize,
) {
    let directory = TempDir::new().unwrap();
    let engine = open(provider, &directory);
    exec(&engine, include_str!("after_events.sql"));
    engine.set_query_memory_limit(Some(64 * 1024));
    exec(&engine, "UPDATE parents SET a=a+10,b=b+100");
    // Independently observed on PostgreSQL 18.4; each cascade queues its original row image.
    let expected = [
        "R:1:1:1>11:1:32768:6",
        "R:2:1:1>11:1:32768:6",
        "R:1:11:1>11:101:32768:6",
        "R:2:11:1>11:101:32768:6",
        "R:3:2:2>12:2:32768:6",
        "R:3:12:2>12:102:32768:6",
        "S:6:38:8>68:308",
    ];
    let audit = "SELECT message FROM audit ORDER BY seq";
    assert_eq!(strings(&engine, audit, "message"), expected);
    exec(&engine, "BEGIN; UPDATE parents SET a=a+100; ROLLBACK");
    assert_eq!(strings(&engine, audit, "message"), expected);
    assert_eq!(
        strings(
            &engine,
            "SELECT id||':'||a||':'||b||':'||length(body) AS message FROM children ORDER BY id",
            "message"
        ),
        ["1:11:101:32768", "2:11:101:32768", "3:12:102:32768"],
    );
    drop(engine);
    if provider != 0 {
        let engine = open(provider, &directory);
        assert_eq!(strings(&engine, audit, "message"), expected);
        assert_eq!(
            exec(&engine, "SELECT count(*) FROM children").value_at(0, 0),
            Some(&Value::Int(3))
        );
    }
}
