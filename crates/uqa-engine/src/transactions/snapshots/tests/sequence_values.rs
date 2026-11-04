//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};

#[rstest::rstest]
#[case("COMMIT")]
#[case("ROLLBACK")]
fn transaction_sequence_cuts_join_reserved_values_to_original_incarnations(
    #[case] completion: &str,
) {
    let engine = Engine::new();
    sql(
        &engine,
        "CREATE SEQUENCE saved START 1 INCREMENT 3 CACHE 4; SELECT nextval('saved')",
    );
    let relation = RelationIdentity::new("public", "saved");
    let original = engine.durable.sequences.read()[&relation];
    assert_eq!(original.current, 10);
    let original_id = engine.durable.sequence_object_ids.read()[&relation];
    sql(&engine, "BEGIN");
    for _ in 0..4 {
        sql(&engine, "SELECT nextval('saved')");
    }
    assert_eq!(engine.durable.sequences.read()[&relation].current, 22);
    sql(&engine, "SAVEPOINT prior_definition; ALTER SEQUENCE saved RESTART 100; SELECT nextval('saved'); ALTER SEQUENCE saved RENAME TO changed; CREATE SEQUENCE saved START 900; SELECT nextval('saved'); CREATE TEMP SEQUENCE temporary_sequence; SELECT nextval('temporary_sequence')");
    let control = StorageReadControl::with_limit(64 << 10);
    let first = capture(&engine, &control);
    assert!(first.sequences.retained(&first.cut).unwrap().is_none());
    sql(
        &engine,
        "ROLLBACK TO prior_definition; SELECT setval('saved',2,false)",
    );
    let later_control = StorageReadControl::with_limit(64 << 10);
    let second = capture(&engine, &later_control);
    sql(&engine, completion);
    let final_state = engine.durable.sequences.read()[&relation];
    assert_eq!(final_state.current, 2);
    assert!(!final_state.called);
    sql(&engine, "SELECT nextval('saved'); DROP SEQUENCE saved; CREATE SEQUENCE saved START 2000; SELECT nextval('saved')");
    drop(engine);
    let saved = first.sequences.captured(&first.cut).unwrap();
    let rows = saved.get().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].relation, relation);
    assert_eq!(rows[0].object_id, original_id);
    assert_eq!(
        rows[0].state.definition_generation,
        original.definition_generation
    );
    assert_eq!(rows[0].state.increment, original.increment);
    assert_eq!(rows[0].state.cache_size, 4);
    assert_eq!(rows[0].state.current, 22);
    assert!(rows[0].state.called);
    let later = second.sequences.captured(&second.cut).unwrap();
    assert_eq!(later.get().unwrap()[0].state.current, 2);
    assert!(!later.get().unwrap()[0].state.called);
    drop((first, second));
    control.cancellation().cancel();
    assert!(saved.get().is_err());
    assert_eq!(later.get().unwrap()[0].state.current, 2);
    drop((saved, later));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(later_control.memory().used(), 0);
}

#[test]
fn transaction_sequence_values_capture_without_waiting_for_a_private_callback() {
    let engine = Arc::new(Engine::new());
    sql(&engine, "CREATE SEQUENCE saved START 5 CACHE 8");
    let original_generation = engine.durable.sequences.read()
        [&RelationIdentity::new("public", "saved")]
        .definition_generation;
    let (entered, started) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let writer = engine.clone();
    let worker = std::thread::spawn(move || {
        writer.transaction(|engine| {
            sql(
                engine,
                "SELECT nextval('saved'); SAVEPOINT private; ALTER SEQUENCE saved RESTART 600; SELECT nextval('saved')",
            );
            entered.send(()).unwrap();
            proceed.recv().unwrap();
            Ok(())
        })
    });
    started
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let selected = capture(&engine, &control);
    let saved = selected.sequences.captured(&selected.cut).unwrap();
    release.send(()).unwrap();
    worker.join().unwrap().unwrap();
    drop((engine, selected));
    let rows = saved.get().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state.definition_generation, original_generation);
    assert_eq!(rows[0].state.current, 12);
    assert!(rows[0].state.called);
    assert_eq!(rows[0].state.cache_size, 8);
    drop(saved);
    assert_eq!(control.memory().used(), 0);
}
