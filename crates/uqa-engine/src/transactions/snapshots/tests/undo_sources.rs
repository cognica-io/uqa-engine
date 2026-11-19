//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};

fn assert_private_undo(snapshot: &crate::EngineDataSnapshot) {
    assert!(snapshot.data.sources.is_none());
    let table = &snapshot.data.tables[&RelationIdentity::new("public", "docs")];
    assert!(table.document_store.key().is_err());
    assert!(table.columns.key().is_err());
}

#[rstest::rstest]
#[case(true)]
#[case(false)]
fn private_undo_generations_preserve_rows_and_sequences_without_capture_registration(
    #[case] commit_nested: bool,
) {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE docs(id integer PRIMARY KEY, body text); INSERT INTO docs VALUES(1,'committed'); CREATE SEQUENCE saved; SELECT nextval('saved')");
    engine.begin().unwrap();
    sql(
        &engine,
        "UPDATE docs SET body='savepoint'; SELECT nextval('saved'); SAVEPOINT saved_rows",
    );
    {
        let frames = engine.session.transactions.lock();
        let outer = frames[0].data_snapshot.as_ref().unwrap();
        assert!(outer.data.sources.is_some());
        assert!(outer.data.tables[&RelationIdentity::new("public", "docs")]
            .document_store
            .key()
            .is_ok());
        assert_private_undo(frames[0].savepoints[0].data_snapshot.as_ref().unwrap());
    }
    engine.begin().unwrap();
    sql(&engine, "UPDATE docs SET body='nested'; ALTER TABLE docs ADD COLUMN extra integer; SELECT nextval('saved'); SAVEPOINT nested_rows");
    {
        let frames = engine.session.transactions.lock();
        assert_eq!(frames.len(), 2);
        assert_private_undo(frames[1].data_snapshot.as_ref().unwrap());
        assert_private_undo(frames[1].savepoints[0].data_snapshot.as_ref().unwrap());
        assert!(frames[1].nontransactional_sequence_values.key().is_err());
    }
    let control = StorageReadControl::with_limit(512 << 10);
    let selected = capture(&engine, &control);
    let saved = selected
        .table(&RelationIdentity::new("public", "docs"))
        .unwrap();
    let sequences = selected.sequences.captured(&selected.cut).unwrap();
    assert_eq!(
        saved.document_store.get_field(1, "body").unwrap(),
        Some(Value::Str("committed".into()))
    );
    assert_eq!(saved.columns.get().unwrap().len(), 2);
    assert_eq!(
        sequences
            .get()
            .unwrap()
            .iter()
            .next()
            .unwrap()
            .state
            .current,
        3
    );
    if commit_nested {
        engine.commit().unwrap();
    } else {
        engine.rollback().unwrap();
    }
    assert_eq!(engine.transaction_depth(), 1);
    engine.rollback_to_savepoint("saved_rows").unwrap();
    assert_eq!(
        engine
            .sql("SELECT body FROM docs", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Str("savepoint".into()))
    );
    assert_eq!(
        engine.table("docs").unwrap().unwrap().columns.read().len(),
        2
    );
    assert_eq!(
        engine
            .sql("SELECT nextval('saved')", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(4))
    );
    engine.rollback().unwrap();
    assert_eq!(
        engine
            .sql("SELECT body FROM docs", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Str("committed".into()))
    );
    assert_eq!(
        engine
            .sql("SELECT nextval('saved')", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(5))
    );
    drop(engine);
    assert_eq!(
        saved.document_store.get_field(1, "body").unwrap(),
        Some(Value::Str("committed".into()))
    );
    assert_eq!(
        sequences
            .get()
            .unwrap()
            .iter()
            .next()
            .unwrap()
            .state
            .current,
        3
    );
    drop(selected);
    control.cancellation().cancel();
    assert!(saved.document_store.len().is_err());
    assert!(sequences.get().is_err());
    drop((saved, sequences));
    assert_eq!(control.memory().used(), 0);
}
