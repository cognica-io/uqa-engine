//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::tests::providers::Provider;
use uqa_ml::{DeepLayerSpec, DeepModel, GatingSpec};

const NAME: &str = "stored_model\0한글";

fn model(alpha: f64) -> DeepModel {
    DeepModel {
        layers: vec![DeepLayerSpec::Embed {
            embedding: vec![-0.0; 3073],
        }],
        alpha,
        gating: GatingSpec::ReLU,
    }
}

fn assert_model(engine: &Engine, expected: &DeepModel) {
    let actual = engine.load_model(NAME).unwrap().unwrap();
    assert_eq!(&actual, expected);
    let DeepLayerSpec::Embed { embedding } = &actual.layers[0] else {
        panic!("stored embedding");
    };
    assert!(embedding
        .iter()
        .all(|value| value.to_bits() == (-0.0_f64).to_bits()));
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_models_keep_full_and_incremental_values_after_source_loss_and_rollback(
    #[case] provider: Provider,
) {
    for incremental in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let source = provider.open(&directory.path().join("source"));
        source.save_model(NAME, &model(0.25)).unwrap();
        let parent = capture(&source, &repository, None);
        let (root, expected) = if incremental {
            let selected = model(0.5);
            source.save_model(NAME, &selected).unwrap();
            (
                capture(&source, &repository, Some(parent.backup())),
                selected,
            )
        } else {
            (parent, model(0.25))
        };
        source.save_model(NAME, &model(0.75)).unwrap();
        drop(source);
        let path = directory.path().join("restored");
        let control = StorageReadControl::with_limit(32 << 20);
        let authority = Engine::new();
        let mut job = authority
            .begin_restore(
                &repository,
                root.backup(),
                providers::destination(provider, &path),
                RestoreOptions::new(16 << 20, 8 << 20),
                &control,
            )
            .unwrap();
        prepare(&mut job);
        let restored = job.publish().unwrap();
        drop(job);
        repository.remove_backup(root.backup(), &control).unwrap();
        if incremental {
            repository.remove_backup(parent.backup(), &control).unwrap();
        }
        drop(repository);
        assert_model(&restored, &expected);
        let retained = restored.durable.models.read().get(NAME).unwrap().clone();
        restored.sql("BEGIN", &[]).unwrap();
        restored.save_model(NAME, &model(0.125)).unwrap();
        restored.sql("ROLLBACK", &[]).unwrap();
        assert_model(&restored, &expected);
        control.cancellation().cancel();
        restored.save_model(NAME, &model(0.0625)).unwrap();
        assert_eq!(&*retained, &expected);
        drop((retained, restored));
        assert_eq!(control.memory().used(), 0);
        if !matches!(provider, Provider::Memory) {
            assert_model(&provider.open(&path.join("database")), &model(0.0625));
        }
    }
}
