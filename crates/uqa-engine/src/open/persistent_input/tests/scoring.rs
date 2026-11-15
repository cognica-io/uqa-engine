//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn reach_scoring(input: &mut PersistentEngineInput) {
    for _ in 0..65536 {
        if matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::Caches(CacheInput::Scoring(_))
        ) {
            return;
        }
        assert!(input.advance(1).unwrap().is_none());
    }
    panic!("scoring catalog input was not reached");
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_scoring_catalog_loading_yields_before_complete_cache_publication(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("scoring"));
    let value = "opaque\0한🙂".repeat(4096);
    for index in 0..16 {
        source
            .save_scoring_params(&format!("signal-{index:03}"), &value)
            .unwrap();
    }
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    for _ in 0..65536 {
        if matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::Caches(CacheInput::Models)
        ) {
            break;
        }
        assert!(input.advance(1).unwrap().is_none());
    }
    assert!(matches!(
        input.candidate.as_ref().unwrap().stage,
        Stage::Caches(CacheInput::Models)
    ));
    assert!(
        input.advance(1).unwrap().is_none(),
        "a complete scoring catalog was loaded in one private restoration poll"
    );
    let mut polls = 0;
    let session = loop {
        polls += 1;
        assert!(polls < 65536);
        if let Some(session) = input.advance(1).unwrap() {
            break session;
        }
    };
    assert!(polls > 16);
    for index in 0..16 {
        assert_eq!(
            session
                .engine()
                .durable
                .scoring_params
                .read()
                .get(&format!("signal-{index:03}")),
            Some(&value)
        );
    }
    drop(source);
    let restored = session.finish().unwrap();
    let retained = restored.durable.scoring_params.read().clone();
    let pointer = retained.get("signal-000").unwrap().as_ptr();
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    restored
        .save_scoring_params("signal-000", "changed")
        .unwrap();
    assert_eq!(retained.get("signal-000"), Some(&value));
    assert_eq!(retained.get("signal-000").unwrap().as_ptr(), pointer);
    drop((restored, retained, input));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_scoring_input_keeps_one_snapshot_and_retires_every_terminal_transaction(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("scoring"));
    let value = "opaque\0한🙂".repeat(2048);
    for index in 0..16 {
        source
            .save_scoring_params(&format!("signal-{index:03}"), &value)
            .unwrap();
    }
    for terminal in ["complete", "cancel", "memory", "zero", "drop", "unwind"] {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&source, &control);
        reach_scoring(&mut input);
        for _ in 0..16 {
            assert!(input.advance(1).unwrap().is_none());
        }
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
        if terminal == "complete" {
            source.save_scoring_params("signal-000", "later").unwrap();
            let session = finish(&mut input);
            assert_eq!(
                session
                    .engine()
                    .durable
                    .scoring_params
                    .read()
                    .get("signal-000"),
                Some(&value)
            );
            drop((session, input));
            assert!(!backend.in_transaction());
            assert_eq!(control.memory().used(), 0);
            source.save_scoring_params("signal-000", &value).unwrap();
            continue;
        }
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
                    let _input = input;
                    panic!("retire private scoring restoration");
                }))
                .is_err()
            );
        } else {
            if terminal != "drop" {
                let mut rejected = false;
                for _ in 0..65536 {
                    match input.advance(usize::from(terminal != "zero")) {
                        Err(_) => {
                            rejected = true;
                            break;
                        }
                        Ok(None) => (),
                        Ok(Some(_)) => panic!("terminal scoring restoration completed"),
                    }
                }
                assert!(rejected);
                assert!(input.candidate.is_none());
                assert!(input.advance(1).is_err());
            }
            drop(input);
        }
        drop(occupied);
        assert!(!backend.in_transaction());
        assert_eq!(control.memory().used(), 0, "{terminal}");
        assert_eq!(
            source.load_scoring_params("signal-000").unwrap(),
            Some(value.clone())
        );
    }
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_scoring_input_rejects_oversized_records_without_publishing_a_private_engine(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("scoring"));
    let value = "x".repeat(LIMITS.maximum_value_bytes + 1);
    source.save_scoring_params("signal", &value).unwrap();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    reach_scoring(&mut input);
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
    let mut rejected = false;
    for _ in 0..65536 {
        match input.advance(1) {
            Err(_) => {
                rejected = true;
                break;
            }
            Ok(None) => (),
            Ok(Some(_)) => panic!("oversized scoring input completed"),
        }
    }
    assert!(rejected);
    assert!(input.candidate.is_none());
    assert!(input.advance(1).is_err());
    assert!(!backend.in_transaction());
    drop(input);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(source.load_scoring_params("signal").unwrap(), Some(value));
}
