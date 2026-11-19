//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn fixture(engine: &Engine) {
    engine.sql("CREATE TABLE docs(id integer PRIMARY KEY, body text, title text); CREATE INDEX docs_text ON docs USING gin (body, title)", &[]).unwrap();
    engine
        .register_named_analyzer("field_index", r#"{"tokenizer":{"type":"whitespace"}}"#)
        .unwrap();
    engine
        .register_named_analyzer("field_search", r#"{"tokenizer":{"type":"keyword"}}"#)
        .unwrap();
    engine
        .set_table_field_analyzer("docs", "body", "field_index", "index")
        .unwrap();
    engine
        .set_table_field_analyzer("docs", "body", "field_search", "search")
        .unwrap();
    engine
        .sql("INSERT INTO docs VALUES(1,'A B','title')", &[])
        .unwrap();
}

fn reach(input: &mut PersistentEngineInput, installation: bool) {
    for _ in 0..1_000_000 {
        let phase = &input.candidate.as_ref().unwrap().stage;
        if matches!(
            (phase, installation),
            (
                Stage::FieldAnalyzers(FieldAnalyzers::Bindings { .. }),
                false
            ) | (
                Stage::FieldAnalyzers(FieldAnalyzers::Installation { .. }),
                true
            )
        ) {
            return;
        }
        assert!(input.advance(1).unwrap().is_none());
    }
    panic!("persistent field analyzer phase was not reached")
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_field_installation_keeps_selected_sides_and_writable_later_state(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("fields"));
    fixture(&source);
    let key = ("public.docs".into(), "body".into());
    let original = source
        .durable
        .table_field_analyzers
        .read()
        .get(&key)
        .unwrap()
        .to_json()
        .unwrap();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    reach(&mut input, false);
    assert!(input.advance(3).unwrap().is_none());
    source
        .register_named_analyzer("field_index", r#"{"tokenizer":{"type":"keyword"}}"#)
        .unwrap();
    reach(&mut input, true);
    assert!(input
        .candidate
        .as_ref()
        .unwrap()
        .session
        .engine()
        .durable
        .table_field_analyzers
        .read()
        .is_empty());
    assert!(input.advance(1).unwrap().is_none());
    let session = finish(&mut input);
    {
        let fields = session.engine().durable.table_field_analyzers.read();
        let field = fields.get(&key).unwrap();
        assert_eq!(field.to_json().unwrap(), original);
        assert_eq!(field.index.compiled.analyze("A B").unwrap(), ["A", "B"]);
        assert_eq!(field.search.compiled.analyze("A B").unwrap(), ["A B"]);
    }
    drop(source);
    let restored = session.finish().unwrap();
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    restored.sql("INSERT INTO docs VALUES(2,'A C','other'); COMMIT; BEGIN; INSERT INTO docs VALUES(3,'A D','rolled back'); ROLLBACK", &[]).unwrap();
    let result = restored
        .sql(
            "SELECT id FROM docs WHERE text_match(body, 'A') ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        [Value::Int(1), Value::Int(2)]
    );
    drop((restored, input, result));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_field_input_and_installation_retire_private_roots_and_transactions(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("fields"));
    fixture(&source);
    let before = source
        .storage
        .catalog
        .as_ref()
        .unwrap()
        .load_table_field_analyzer_bindings()
        .unwrap();
    for installation in [false, true] {
        for terminal in ["cancel", "memory", "zero", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(32 << 20);
            let mut input = input(&source, &control);
            reach(&mut input, installation);
            assert!(input.advance(1).unwrap().is_none());
            let backend = Arc::clone(
                input
                    .candidate
                    .as_ref()
                    .unwrap()
                    .session
                    .engine()
                    .storage
                    .backend
                    .as_ref()
                    .unwrap(),
            );
            let occupied = (terminal == "memory").then(|| {
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap()
            });
            if terminal == "cancel" {
                control.cancellation().cancel();
            }
            if terminal == "unwind" {
                assert!(
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _owner = input;
                        panic!("abandoned field restoration");
                    }))
                    .is_err()
                );
            } else {
                if terminal != "drop" {
                    loop {
                        match input.advance(usize::from(terminal != "zero")) {
                            Err(_) => break,
                            Ok(None) => (),
                            Ok(Some(_)) => panic!("terminal field restoration completed"),
                        }
                    }
                    assert!(input.advance(1).is_err());
                }
                drop(input);
            }
            drop(occupied);
            assert!(!backend.in_transaction());
            assert_eq!(control.memory().used(), 0);
        }
    }
    assert_eq!(
        source
            .storage
            .catalog
            .as_ref()
            .unwrap()
            .load_table_field_analyzer_bindings()
            .unwrap(),
        before
    );
    source
        .sql("INSERT INTO docs VALUES(2,'later','usable')", &[])
        .unwrap();
}
