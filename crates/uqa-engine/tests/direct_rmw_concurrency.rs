//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Direct public read/modify/write APIs must reserve the persistent writer
//! before refreshing their engine-local snapshot. Otherwise independently
//! opened sessions can both derive from the same old value and the last full
//! snapshot write silently discards the first mutation.

use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use tempfile::tempdir;
use uqa_core::Vertex;
use uqa_engine::Engine;
use uqa_graph::GraphStore as _;
use uqa_ml::{DeepLayerSpec, DeepModel, GatingSpec};

#[test]
fn concurrent_graph_vertex_mutations_from_independent_engines_do_not_lose_updates() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("concurrent-graph-rmw.db");
    Engine::open(&database).unwrap().create_graph("g").unwrap();

    // Open both sessions before either mutation so both begin with the same
    // empty engine-local graph cache.
    let first = Engine::open(&database).unwrap();
    let second = Engine::open(&database).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let handles = [(first, 1_u64), (second, 2_u64)]
        .into_iter()
        .map(|(engine, vertex_id)| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                engine.add_graph_vertex(Vertex::new(vertex_id, "P"), "g")
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }

    let reopened = Engine::open(&database).unwrap();
    let vertices = reopened
        .graph_with("g", |store| store.vertex_ids_in_graph("g").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(vertices.into_iter().collect::<Vec<_>>(), vec![1, 2]);
}

#[test]
fn concurrent_scoring_updates_from_independent_engines_do_not_lose_training_steps() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("concurrent-scoring-rmw.db");
    {
        let bootstrap = Engine::open(&database).unwrap();
        bootstrap
            .create_default_table("docs", vec!["body".into()])
            .unwrap();
    }

    let first = Engine::open(&database).unwrap();
    let second = Engine::open(&database).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let handles = [first, second]
        .into_iter()
        .map(|engine| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                engine.update_scoring_params("docs", "body", 2.0, 1)
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }

    let expected = Engine::new();
    expected
        .create_default_table("docs", vec!["body".into()])
        .unwrap();
    expected
        .update_scoring_params("docs", "body", 2.0, 1)
        .unwrap();
    expected
        .update_scoring_params("docs", "body", 2.0, 1)
        .unwrap();
    let expected_json = expected.load_scoring_params("docs.body").unwrap().unwrap();

    let reopened = Engine::open(&database).unwrap();
    let actual_json = reopened.load_scoring_params("docs.body").unwrap().unwrap();
    let expected_value: serde_json::Value = serde_json::from_str(&expected_json).unwrap();
    let actual_value: serde_json::Value = serde_json::from_str(&actual_json).unwrap();
    assert_eq!(actual_value, expected_value);
}

#[test]
fn concurrent_independent_catalog_writes_all_survive_reopen() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("concurrent-catalog-writes.db");
    Engine::open(&database).unwrap();

    let first = Engine::open(&database).unwrap();
    let second = Engine::open(&database).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let handles = [(first, "one", 1_i64), (second, "two", 2_i64)]
        .into_iter()
        .map(|(engine, suffix, seed)| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                let analyzer = format!("analyzer_{suffix}");
                engine.register_named_analyzer(
                    &analyzer,
                    r#"{"tokenizer":{"type":"standard"},"token_filters":[]}"#,
                )?;
                engine
                    .create_default_table(format!("table_{suffix}"), Vec::new())
                    .map_err(|error| error.to_string())?;
                engine.create_sequence(&format!("sequence_{suffix}"), seed, 1, false)?;
                let model = DeepModel {
                    layers: vec![DeepLayerSpec::Embed {
                        embedding: vec![seed as f64],
                    }],
                    alpha: 0.0,
                    gating: GatingSpec::None,
                };
                engine
                    .save_model(&format!("model_{suffix}"), &model)
                    .map_err(|error| error.to_string())?;
                engine
                    .save_scoring_params(
                        &format!("params_{suffix}"),
                        &format!("{{\"seed\":{seed}}}"),
                    )
                    .map_err(|error| error.to_string())?;
                Ok::<(), String>(())
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }

    let reopened = Engine::open(&database).unwrap();
    for (suffix, seed) in [("one", 1_i64), ("two", 2_i64)] {
        assert!(reopened
            .list_named_analyzers()
            .unwrap()
            .contains(&format!("analyzer_{suffix}")));
        assert!(reopened.has_table(&format!("table_{suffix}")).unwrap());
        assert_eq!(
            reopened
                .sequence_state(&format!("sequence_{suffix}"))
                .unwrap()
                .unwrap()
                .1
                .start,
            seed
        );
        assert!(reopened
            .load_model(&format!("model_{suffix}"))
            .unwrap()
            .is_some());
        assert_eq!(
            reopened
                .load_scoring_params(&format!("params_{suffix}"))
                .unwrap(),
            Some(format!("{{\"seed\":{seed}}}"))
        );
    }
}

#[test]
fn auto_estimation_rechecks_params_after_waiting_for_the_writer() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("auto-estimate-rmw.db");
    {
        let bootstrap = Engine::open(&database).unwrap();
        bootstrap
            .sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", &[])
            .unwrap();
        bootstrap
            .sql("CREATE INDEX docs_body_idx ON docs USING gin (body)", &[])
            .unwrap();
        bootstrap
            .sql(
                "INSERT INTO docs VALUES (1, 'writer lock calibration')",
                &[],
            )
            .unwrap();
    }

    let manual = Engine::open(&database).unwrap();
    let automatic = Engine::open(&database).unwrap();
    let writer_ready = Arc::new(Barrier::new(2));
    let manual_ready = Arc::clone(&writer_ready);
    let custom = r#"{"alpha":7.0,"beta":-2.0,"base_rate":0.25}"#.to_string();
    let custom_for_writer = custom.clone();
    let writer = thread::spawn(move || {
        manual.transaction(|engine| {
            manual_ready.wait();
            // Give the automatic reader enough time to observe the missing
            // value and queue for the writer lock held by this transaction.
            thread::sleep(Duration::from_millis(200));
            engine.save_scoring_params("docs.body", &custom_for_writer)
        })
    });

    writer_ready.wait();
    let resolved = automatic.bayesian_params_for("docs", "body").unwrap();
    writer.join().unwrap().unwrap();

    assert_eq!(resolved.alpha, 7.0);
    assert_eq!(resolved.beta, -2.0);
    assert_eq!(resolved.base_rate, 0.25);
    assert_eq!(
        Engine::open(&database)
            .unwrap()
            .load_scoring_params("docs.body")
            .unwrap(),
        Some(custom)
    );
}
