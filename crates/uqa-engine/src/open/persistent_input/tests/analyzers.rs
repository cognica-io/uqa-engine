//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn reach_named(input: &mut PersistentEngineInput) {
    for _ in 0..1_000_000 {
        if matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::NamedAnalyzers(_)
        ) {
            return;
        }
        assert!(input.advance(1).unwrap().is_none());
    }
    panic!("named analyzer stage was not reached")
}
#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_named_analyzers_keep_selected_revisions_and_allow_later_mutations(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("analyzers"));
    source
        .register_named_analyzer("selected", r#"{"tokenizer":{"type":"whitespace"}}"#)
        .unwrap();
    let original = source
        .durable
        .named_analyzers
        .read()
        .get("selected")
        .unwrap()
        .clone();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    reach_named(&mut input);
    assert!(input.advance(3).unwrap().is_none());
    source
        .register_named_analyzer("selected", r#"{"tokenizer":{"type":"keyword"}}"#)
        .unwrap();
    source
        .register_named_analyzer("later", r#"{"tokenizer":{"type":"whitespace"}}"#)
        .unwrap();
    let session = finish(&mut input);
    {
        let names = session.engine().durable.named_analyzers.read();
        assert_eq!(names.len(), 1);
        assert_eq!(
            names.get("selected").unwrap().descriptor().canonical_json(),
            original.descriptor().canonical_json()
        );
    }
    let restored = session.finish().unwrap();
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    restored
        .register_named_analyzer("after_input", r#"{"tokenizer":{"type":"keyword"}}"#)
        .unwrap();
    assert!(restored
        .durable
        .named_analyzers
        .read()
        .contains_key("after_input"));
    drop((restored, input, source, original));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}
#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_named_analyzers_retire_private_publication_and_selected_transactions(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("analyzers"));
    for index in 0..16 {
        source
            .register_named_analyzer(
                &format!("custom_{index}"),
                r#"{"tokenizer":{"type":"whitespace"}}"#,
            )
            .unwrap();
    }
    let before = source
        .storage
        .catalog
        .as_ref()
        .unwrap()
        .load_analyzer_descriptors()
        .unwrap();
    for terminal in ["cancel", "memory", "zero", "drop", "unwind"] {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&source, &control);
        reach_named(&mut input);
        assert!(input.advance(17).unwrap().is_none());
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
                    panic!("abandoned private analyzer input");
                }))
                .is_err()
            );
        } else {
            if terminal != "drop" {
                loop {
                    match input.advance(usize::from(terminal != "zero")) {
                        Err(_) => break,
                        Ok(None) => (),
                        Ok(Some(_)) => panic!("terminal input completed"),
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
    assert_eq!(
        source
            .storage
            .catalog
            .as_ref()
            .unwrap()
            .load_analyzer_descriptors()
            .unwrap(),
        before
    );
    source
        .register_named_analyzer("after_failure", r#"{"tokenizer":{"type":"whitespace"}}"#)
        .unwrap();
}
