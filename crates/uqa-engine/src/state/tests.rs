//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::{EpochCoordinator, SessionContext};

#[test]
fn session_state_readers_never_observe_a_torn_pair() {
    let session = Arc::new(SessionContext::new(crate::random_state_from_seed(1)));
    {
        let mut state = session.state.write();
        state.search_path = vec!["0".to_string()];
        state.session_vars.insert("generation".into(), "0".into());
    }
    let done = Arc::new(AtomicBool::new(false));
    let writer_session = Arc::clone(&session);
    let writer_done = Arc::clone(&done);
    let writer = std::thread::spawn(move || {
        for generation in 1..10_000 {
            let generation = generation.to_string();
            let mut state = writer_session.state.write();
            state.search_path = vec![generation.clone()];
            state.session_vars.insert("generation".into(), generation);
        }
        writer_done.store(true, Ordering::Release);
    });

    while !done.load(Ordering::Acquire) {
        let state = session.state.read();
        assert_eq!(
            state.search_path.first(),
            state.session_vars.get("generation")
        );
    }
    writer.join().unwrap();
}

#[test]
fn derived_epoch_coordinator_shares_only_publication() {
    let source = EpochCoordinator::new();
    source.table_data.published.store(7, Ordering::Release);
    source.table_data.seen.store(6, Ordering::Release);
    source.table_data.dirty.store(true, Ordering::Release);
    let observed = source.published_epochs();
    source.table_data.published.store(8, Ordering::Release);

    let mut derived = EpochCoordinator::new();
    derived.share_published_from_at(&source, observed);

    assert!(Arc::ptr_eq(
        &source.table_data.published,
        &derived.table_data.published
    ));
    assert_eq!(derived.table_data.seen.load(Ordering::Acquire), 7);
    assert_eq!(derived.table_data.published.load(Ordering::Acquire), 8);
    assert!(!derived.table_data.dirty.load(Ordering::Acquire));
    derived.table_data.dirty.store(true, Ordering::Release);
    assert!(source.table_data.dirty.load(Ordering::Acquire));
    derived.table_data.dirty.store(false, Ordering::Release);
    assert!(source.table_data.dirty.load(Ordering::Acquire));
}

#[test]
fn model_catalog_publication_shares_retained_weights_without_payload_copies() {
    use uqa_ml::{DeepLayerSpec, DeepModel, GatingSpec};

    let engine = crate::Engine::new();
    let large = DeepModel {
        layers: vec![DeepLayerSpec::Embed {
            embedding: vec![0.25; 256 * 1024],
        }],
        alpha: 0.5,
        gating: GatingSpec::None,
    };
    engine.save_model("large", &large).unwrap();
    drop(large);
    let mut captured = None;
    let capture = allocation_counter::measure(|| captured = Some(engine.durable.snapshot()));
    assert_eq!(capture.bytes_total, 0);
    let captured = captured.unwrap();
    let small = DeepModel {
        layers: vec![DeepLayerSpec::Input { dimensions: 1 }],
        alpha: 0.5,
        gating: GatingSpec::None,
    };
    let publication = allocation_counter::measure(|| engine.save_model("small", &small).unwrap());
    assert!(publication.bytes_total < 64 * 1024, "{publication:?}");
    assert!(Arc::ptr_eq(
        &captured.models["large"],
        &engine.durable.models.read()["large"]
    ));
    assert!(!captured.models.contains_key("small"));
    engine.save_model("large", &small).unwrap();
    assert!(!Arc::ptr_eq(
        &captured.models["large"],
        &engine.durable.models.read()["large"]
    ));
    drop(engine);
    let DeepLayerSpec::Embed { embedding } = &captured.models["large"].layers[0] else {
        panic!("retained model was replaced")
    };
    assert_eq!(embedding.len(), 256 * 1024);
    assert!(embedding.iter().all(|value| *value == 0.25));
}

#[test]
fn model_catalog_generations_preserve_rollback_and_owned_public_results() {
    use uqa_ml::{DeepLayerSpec, DeepModel, GatingSpec};

    let engine = crate::Engine::new();
    let original = DeepModel {
        layers: vec![DeepLayerSpec::Input { dimensions: 1 }],
        alpha: 0.5,
        gating: GatingSpec::None,
    };
    engine.save_model("original", &original).unwrap();
    let captured = engine.durable.snapshot();
    engine.sql("BEGIN", &[]).unwrap();
    let mut replacement = original.clone();
    replacement.alpha = 0.75;
    engine.save_model("original", &replacement).unwrap();
    engine.save_model("private", &replacement).unwrap();
    assert_eq!(captured.models["original"].alpha, 0.5);
    engine.sql("ROLLBACK", &[]).unwrap();
    assert!(Arc::ptr_eq(
        &captured.models["original"],
        &engine.durable.models.read()["original"]
    ));
    assert!(engine.load_model("private").unwrap().is_none());
    let mut returned = engine.load_model("original").unwrap().unwrap();
    returned.layers.clear();
    assert_eq!(
        engine.load_model("original").unwrap().unwrap().layers.len(),
        1
    );
    assert!(engine.drop_model("original").unwrap());
    assert_eq!(captured.models["original"].layers.len(), 1);
    assert!(engine.load_model("original").unwrap().is_none());
}

#[test]
fn model_catalog_generations_preserve_all_provider_publication_and_reopen() {
    use crate::tests::providers::Provider;
    use uqa_ml::{DeepLayerSpec, DeepModel, GatingSpec};

    for provider in [
        Provider::Memory,
        Provider::SQLiteNative,
        Provider::SQLiteKeyValue,
        Provider::Redb,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("models.db");
        let engine = provider.open(&path);
        let original = DeepModel {
            layers: vec![
                DeepLayerSpec::Input { dimensions: 1 },
                DeepLayerSpec::Dense {
                    weights: vec![0.25],
                    bias: vec![0.0],
                    output_channels: 1,
                    input_channels: 1,
                },
            ],
            alpha: 0.5,
            gating: GatingSpec::None,
        };
        engine.save_model("retained", &original).unwrap();
        let first = engine.durable.snapshot();
        engine.sql("BEGIN", &[]).unwrap();
        let mut replacement = original.clone();
        replacement.alpha = 0.75;
        engine.save_model("retained", &replacement).unwrap();
        engine.save_model("private", &replacement).unwrap();
        engine.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(
            engine.load_model("retained").unwrap(),
            Some(original.clone())
        );
        assert!(engine.load_model("private").unwrap().is_none());
        if matches!(provider, Provider::Memory) {
            engine.save_model("retained", &replacement).unwrap();
        } else {
            let peer = engine.new_session().unwrap();
            peer.save_model("retained", &replacement).unwrap();
            engine.sql("SELECT 1", &[]).unwrap();
            assert_eq!(
                engine.load_model("retained").unwrap(),
                Some(replacement.clone())
            );
        }
        let predictions = engine
            .deep_predict_features("retained", &[(7, vec![2.0])])
            .unwrap();
        assert_eq!(*first.models["retained"], original);
        drop(engine);
        assert_eq!(*first.models["retained"], original);
        if !matches!(provider, Provider::Memory) {
            let reopened = provider.open(&path);
            assert_eq!(reopened.load_model("retained").unwrap(), Some(replacement));
            assert_eq!(
                reopened
                    .deep_predict_features("retained", &[(7, vec![2.0])])
                    .unwrap(),
                predictions
            );
            assert!(reopened.load_model("private").unwrap().is_none());
            assert!(reopened.drop_model("retained").unwrap());
            drop(reopened);
            assert!(provider
                .open(&path)
                .load_model("retained")
                .unwrap()
                .is_none());
        }
    }
}
