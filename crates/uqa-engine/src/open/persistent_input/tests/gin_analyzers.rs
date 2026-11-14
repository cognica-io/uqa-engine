//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn fixture(engine: &Engine) {
    engine
        .register_named_analyzer("gin_saved", r#"{"tokenizer":{"type":"whitespace"}}"#)
        .unwrap();
    engine.sql("CREATE TABLE docs(id integer PRIMARY KEY, body text); CREATE INDEX docs_text ON docs USING gin(body) WITH (analyzer='gin_saved'); INSERT INTO docs VALUES(1,'A B')",&[]).unwrap();
}

fn reach(input: &mut PersistentEngineInput) {
    for _ in 0..100_000 {
        if matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::GinAnalyzers(_)
        ) {
            return;
        }
        assert!(input.advance(1).unwrap().is_none());
    }
    panic!("GIN restoration was not reached")
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_gin_input_keeps_selected_owner_and_writable_field_generations(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("gin"));
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
    reach(&mut input);
    assert!(input.advance(3).unwrap().is_none());
    source
        .register_named_analyzer("gin_saved", r#"{"tokenizer":{"type":"keyword"}}"#)
        .unwrap();
    let session = finish(&mut input);
    {
        let fields = session.engine().durable.table_field_analyzers.read();
        let binding = fields.get(&key).unwrap();
        assert_eq!(binding.to_json().unwrap(), original);
        assert_eq!(binding.index.compiled.analyze("A B").unwrap(), ["A", "B"]);
    }
    let restored = session.finish().unwrap();
    drop(source);
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    restored.sql("INSERT INTO docs VALUES(2,'A C'); COMMIT; BEGIN; INSERT INTO docs VALUES(3,'rollback'); ROLLBACK",&[]).unwrap();
    let result = restored
        .sql(
            "SELECT id FROM docs WHERE text_match(body,'A') ORDER BY id",
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
fn persistent_gin_input_retires_private_transaction_after_terminal_failure(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("gin"));
    fixture(&source);
    let before = source
        .storage
        .catalog
        .as_ref()
        .unwrap()
        .load_catalog_indexes()
        .unwrap();
    for terminal in ["cancel", "memory", "zero", "drop", "unwind"] {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&source, &control);
        reach(&mut input);
        assert!(input.advance(1).unwrap().is_none());
        let backend = input
            .candidate
            .as_ref()
            .unwrap()
            .session
            .engine()
            .storage
            .backend
            .as_ref()
            .unwrap()
            .clone();
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
                    panic!("abandoned Engine GIN restoration");
                }))
                .is_err()
            );
        } else {
            if terminal != "drop" {
                loop {
                    match input.advance(usize::from(terminal != "zero")) {
                        Err(error) => {
                            match terminal {
                                "cancel" => {
                                    assert!(matches!(error, StorageBackendError::Cancelled(_)));
                                }
                                "memory" => {
                                    assert!(matches!(error, StorageBackendError::Memory(_)));
                                }
                                "zero" => (),
                                _ => unreachable!(),
                            }
                            break;
                        }
                        Ok(None) => (),
                        Ok(Some(_)) => panic!("interrupted GIN restoration completed"),
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
    let after = source
        .storage
        .catalog
        .as_ref()
        .unwrap()
        .load_catalog_indexes()
        .unwrap();
    assert_eq!(before.len(), after.len());
    assert!(before
        .iter()
        .zip(after)
        .all(|(left, right)| left.columns_json == right.columns_json
            && left.parameters_json == right.parameters_json));
    source
        .sql("INSERT INTO docs VALUES(2,'still usable')", &[])
        .unwrap();
}
