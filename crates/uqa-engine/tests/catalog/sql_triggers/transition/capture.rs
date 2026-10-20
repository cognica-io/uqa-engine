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
