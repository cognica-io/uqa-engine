//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};
use uqa_execution::catalog::identity::CatalogOidCounter;

fn reserve_oid(engine: &Engine) -> u32 {
    engine
        .row_locks
        .catalog_oids()
        .next_oid(None, || panic!("SQL has initialized the counter"))
        .unwrap()
}

#[rstest::rstest]
#[case("COMMIT")]
#[case("ROLLBACK")]
fn transaction_identifier_cuts_preserve_autonomous_oids_and_original_document_floors(
    #[case] completion: &str,
) {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE docs(body text); INSERT INTO docs VALUES('original'); CREATE SEQUENCE saved START 10 CACHE 4");
    let name = RelationIdentity::new("public", "docs");
    let table = engine.table("docs").unwrap().unwrap();
    let object_id = table.object_id();
    let original_floor = *table.next_id.read();
    drop(table);
    sql(&engine, "BEGIN; INSERT INTO docs VALUES('private'); SELECT nextval('saved'); SAVEPOINT nested; CREATE SEQUENCE private_sequence; CREATE TEMP TABLE private_table(payload text); ROLLBACK TO nested");
    assert!(*engine.table("docs").unwrap().unwrap().next_id.read() > original_floor);
    let reserved = reserve_oid(&engine);
    let control = StorageReadControl::with_limit(64 << 10);
    let selected = capture(&engine, &control);
    assert_eq!(reserve_oid(&engine), reserved + 1);
    sql(&engine, completion);
    sql(
        &engine,
        "DROP TABLE docs; CREATE TABLE docs(replacement boolean); CREATE SEQUENCE after_cut",
    );
    drop(engine);
    assert_eq!(
        *selected.catalog_oids.get().unwrap(),
        Some(u64::from(reserved))
    );
    let restored = CatalogOidCounter::from_position(*selected.catalog_oids.get().unwrap()).unwrap();
    assert_eq!(
        restored
            .next_oid(None, || panic!("restored initialized counter"))
            .unwrap(),
        reserved + 1
    );
    let table = selected.table(&name).unwrap();
    assert_eq!(table.object_id, object_id);
    assert_eq!(*table.next_id.get().unwrap(), original_floor);
    assert_eq!(table.document_store.len().unwrap(), 1);
    assert_eq!(
        table.document_store.get_field(1, "body").unwrap(),
        Some(Value::Str("original".into()))
    );
    let sequences = selected.sequences.captured(&selected.cut).unwrap();
    assert_eq!(sequences.get().unwrap().len(), 1);
    assert_eq!(sequences.get().unwrap()[0].state.current, 13);
    control.cancellation().cancel();
    assert!(selected.catalog_oids.get().is_err());
    assert!(sequences.get().is_err());
    drop((selected, table, sequences));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn transaction_identifier_capture_does_not_wait_for_a_private_callback() {
    let engine = Arc::new(Engine::new());
    sql(&engine, "CREATE TABLE docs(body text); INSERT INTO docs VALUES('original'); CREATE SEQUENCE saved START 20 CACHE 4");
    let (entered, started) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let writer = engine.clone();
    let worker = std::thread::spawn(move || {
        writer.transaction(|engine| {
            sql(engine, "SELECT nextval('saved'); CREATE SCHEMA private_schema; CREATE TABLE private_schema.hidden(payload text)");
            entered.send(reserve_oid(engine)).unwrap();
            proceed.recv().unwrap();
            Ok(())
        })
    });
    let reserved = started
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let selected = capture(&engine, &control);
    assert_eq!(
        *selected.catalog_oids.get().unwrap(),
        Some(u64::from(reserved))
    );
    assert_eq!(
        selected.names().unwrap(),
        [RelationIdentity::new("public", "docs")]
    );
    let sequences = selected.sequences.captured(&selected.cut).unwrap();
    assert_eq!(sequences.get().unwrap()[0].state.current, 23);
    release.send(()).unwrap();
    worker.join().unwrap().unwrap();
    drop(engine);
    assert_eq!(
        *selected.catalog_oids.get().unwrap(),
        Some(u64::from(reserved))
    );
    drop((selected, sequences));
    assert_eq!(control.memory().used(), 0);
}
