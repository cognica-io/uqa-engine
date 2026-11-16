//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_ml::{DeepLayerSpec, DeepModel, GatingSpec};

fn reach_models(input: &mut PersistentEngineInput) {
    for _ in 0..65536 {
        if matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::Caches(CacheInput::Models)
        ) {
            return;
        }
        assert!(input.advance(1).unwrap().is_none());
    }
    panic!("model catalog input was not reached");
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_model_catalog_loading_yields_before_complete_cache_publication(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("models"));
    let model = DeepModel {
        layers: vec![DeepLayerSpec::Embed {
            embedding: (0..256).map(|index| f64::from(index) / 256.0).collect(),
        }],
        alpha: 0.25,
        gating: GatingSpec::None,
    };
    for index in 0..16 {
        source
            .save_model(&format!("model-{index:03}"), &model)
            .unwrap();
    }
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    reach_models(&mut input);
    assert!(input.advance(1).unwrap().is_none());
    assert!(
        input
            .candidate
            .as_ref()
            .unwrap()
            .session
            .engine()
            .durable
            .models
            .read()
            .get("model-000")
            .is_none(),
        "the complete model catalog was loaded in one private restoration poll"
    );
    let mut polls = 0;
    let session = loop {
        polls += 1;
        assert!(polls < 65536);
        if let Some(session) = input.advance(64).unwrap() {
            break session;
        }
    };
    assert!(polls > 16);
    for index in 0..16 {
        assert_eq!(
            &**session
                .engine()
                .durable
                .models
                .read()
                .get(&format!("model-{index:03}"))
                .unwrap(),
            &model
        );
    }
    drop(source);
    let restored = session.finish().unwrap();
    let retained = restored
        .durable
        .models
        .read()
        .get("model-000")
        .unwrap()
        .clone();
    let identity = retained.identity();
    let DeepLayerSpec::Embed { embedding } = &retained.layers[0] else {
        panic!("stored embedding");
    };
    let pointer = embedding.as_ptr();
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    restored
        .save_model(
            "model-000",
            &DeepModel {
                layers: vec![],
                alpha: 0.5,
                gating: GatingSpec::Swish,
            },
        )
        .unwrap();
    assert_eq!(&*retained, &model);
    let DeepLayerSpec::Embed { embedding } = &retained.layers[0] else {
        panic!("retained embedding");
    };
    assert_eq!(embedding.as_ptr(), pointer);
    drop((restored, input));
    assert!(identity.is_live());
    drop(retained);
    assert!(!identity.is_live());
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_model_input_keeps_the_snapshot_and_retires_terminal_transactions(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("models"));
    let model = DeepModel {
        layers: vec![DeepLayerSpec::Embed {
            embedding: vec![0.25; 1024],
        }],
        alpha: 0.5,
        gating: GatingSpec::None,
    };
    source.save_model("model", &model).unwrap();
    for terminal in ["complete", "cancel", "memory", "zero", "drop", "unwind"] {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&source, &control);
        reach_models(&mut input);
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
            source
                .save_model(
                    "model",
                    &DeepModel {
                        layers: vec![],
                        alpha: 0.75,
                        gating: GatingSpec::Swish,
                    },
                )
                .unwrap();
            let session = loop {
                if let Some(session) = input.advance(64).unwrap() {
                    break session;
                }
            };
            assert_eq!(
                &**session.engine().durable.models.read().get("model").unwrap(),
                &model
            );
            drop((session, input));
            assert!(!backend.in_transaction());
            assert_eq!(control.memory().used(), 0);
            source.save_model("model", &model).unwrap();
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
                    panic!("retire private model restoration");
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
                        Ok(Some(_)) => panic!("terminal model restoration completed"),
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
        assert_eq!(source.load_model("model").unwrap().as_ref(), Some(&model));
    }
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_model_input_completes_physical_checks_before_semantics_and_rolls_back(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("models"));
    let catalog = source.storage.catalog.as_ref().unwrap();
    catalog
        .save_model("000-invalid", r#"{"layers":[{"kind":"bad"}]}"#)
        .unwrap();
    catalog
        .save_model("zzz-oversized", &" ".repeat(LIMITS.maximum_value_bytes + 1))
        .unwrap();
    for oversized in [true, false] {
        if !oversized {
            catalog.drop_model("zzz-oversized").unwrap();
        }
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&source, &control);
        reach_models(&mut input);
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
        let error = loop {
            match input.advance(64) {
                Err(error) => break error,
                Ok(None) => (),
                Ok(Some(_)) => panic!("invalid model input completed"),
            }
        };
        if oversized {
            assert!(!error.to_string().contains("unknown deep layer"), "{error}");
        } else {
            assert!(
                error.to_string().contains("unknown deep layer kind `bad`"),
                "{error}"
            );
        }
        assert!(input.candidate.is_none());
        assert!(input.advance(1).is_err());
        assert!(!backend.in_transaction());
        drop((error, input));
        assert_eq!(control.memory().used(), 0);
        assert_eq!(
            catalog.load_models().unwrap().len(),
            if oversized { 2 } else { 1 }
        );
    }
}
