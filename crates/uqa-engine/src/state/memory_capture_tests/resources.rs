//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};
use uqa_core::{Value, Vertex};
use uqa_graph::GraphImageSnapshot;
use uqa_ml::{model_image::ModelImageSnapshot, DeepLayerSpec, DeepModel, GatingSpec};
use uqa_storage::StorageBackendError;

fn model(value: f64) -> DeepModel {
    DeepModel {
        layers: vec![DeepLayerSpec::Embed {
            embedding: vec![value; 1 << 17],
        }],
        alpha: value,
        gating: GatingSpec::None,
    }
}

fn populate(engine: &Engine, name: &str, value: f64) {
    let mut vertex = Vertex::new(1, "item");
    vertex
        .properties
        .insert("value".into(), Value::Float(value));
    vertex
        .properties
        .insert("payload".into(), Value::Str("x".repeat(1 << 20)));
    engine.add_graph_vertex(vertex, name).unwrap();
    engine.save_model(name, &model(value)).unwrap();
}

fn graph_image(graph: &GraphImageSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    graph
        .write_image(&mut bytes, &StorageReadControl::with_limit(4096))
        .unwrap();
    bytes
}

fn model_image(model: &ModelImageSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    model
        .write_image(&mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    bytes
}

fn original_images(engine: &Engine) -> (Vec<u8>, Vec<u8>) {
    let control = StorageReadControl::with_limit(4 << 20);
    let graph = engine.durable.graphs.read()["saved"]
        .capture_image_with_control(&control)
        .unwrap();
    let model = engine.durable.models.read()["saved"]
        .capture_image_with_control(&control)
        .unwrap();
    (graph_image(&graph), model_image(&model))
}

#[test]
fn memory_resource_capture_preserves_rollback_replacement_and_unread_engine_closure() {
    for mutate in [false, true] {
        let engine = Engine::new();
        populate(&engine, "saved", 0.25);
        let expected = original_images(&engine);
        let graphs = engine.durable.graphs.key().unwrap();
        let models = engine.durable.models.key().unwrap();
        let control = StorageReadControl::with_limit(8 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        assert!(control.memory().used() < 4096);
        if mutate {
            sql(&engine, "BEGIN");
            populate(&engine, "saved", 0.5);
            sql(&engine, "SAVEPOINT saved_resources");
            engine.drop_graph("saved").unwrap();
            engine.drop_model("saved").unwrap();
            sql(&engine, "ROLLBACK TO saved_resources; ROLLBACK");
            assert_eq!(original_images(&engine), expected);
            engine.drop_graph("saved").unwrap();
            engine.drop_model("saved").unwrap();
            populate(&engine, "saved", 0.75);
            populate(&engine, "later", 1.0);
            assert_ne!(original_images(&engine), expected);
        }
        drop(engine);
        let captured_graphs = graphs.retained(&cut).unwrap().unwrap();
        let captured_models = models.retained(&cut).unwrap().unwrap();
        assert_eq!(captured_graphs.get().unwrap().len(), 1);
        assert_eq!(captured_models.get().unwrap().len(), 1);
        let graph = captured_graphs
            .get()
            .unwrap()
            .image("saved")
            .unwrap()
            .unwrap();
        let model = captured_models
            .get()
            .unwrap()
            .image("saved")
            .unwrap()
            .unwrap();
        drop((cut, captured_graphs, captured_models));
        assert_eq!(graph_image(&graph), expected.0);
        assert_eq!(model_image(&model), expected.1);
        assert!(control.memory().used() >= 2 << 20);
        drop(graph);
        assert!(control.memory().used() >= 1 << 20);
        drop(model);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn memory_resource_capture_failure_preserves_writes_and_independent_capture_allowances() {
    let engine = Engine::new();
    populate(&engine, "saved", 0.25);
    let expected = original_images(&engine);
    let graphs = engine.durable.graphs.key().unwrap();
    let models = engine.durable.models.key().unwrap();
    let small = StorageReadControl::with_limit(4096);
    let good = StorageReadControl::with_limit(4 << 20);
    let registry = engine.storage.memory_capture.as_ref().unwrap();
    let failed = registry.capture(&small).unwrap();
    let cut = registry.capture(&good).unwrap();
    populate(&engine, "saved", 0.5);
    assert!(matches!(
        failed.check(),
        Err(StorageBackendError::Memory(_))
    ));
    assert_ne!(original_images(&engine), expected);
    drop(engine);
    let graph = graphs.retained(&cut).unwrap().unwrap();
    let model = models.retained(&cut).unwrap().unwrap();
    assert_eq!(
        graph_image(&graph.get().unwrap().image("saved").unwrap().unwrap()),
        expected.0
    );
    assert_eq!(
        model_image(&model.get().unwrap().image("saved").unwrap().unwrap()),
        expected.1
    );
    drop((failed, cut, graph, model));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
}

#[test]
fn memory_resource_children_keep_original_cancellation_after_registry_and_engine_loss() {
    let engine = Engine::new();
    populate(&engine, "saved", 0.25);
    let graphs = engine.durable.graphs.key().unwrap();
    let models = engine.durable.models.key().unwrap();
    let control = StorageReadControl::with_limit(4 << 20);
    let cut = engine
        .storage
        .memory_capture
        .as_ref()
        .unwrap()
        .capture(&control)
        .unwrap();
    drop(engine);
    let graph = graphs
        .retained(&cut)
        .unwrap()
        .unwrap()
        .get()
        .unwrap()
        .image("saved")
        .unwrap()
        .unwrap();
    let model = models
        .retained(&cut)
        .unwrap()
        .unwrap()
        .get()
        .unwrap()
        .image("saved")
        .unwrap()
        .unwrap();
    drop(cut);
    control.cancellation().cancel();
    let mut output = Vec::new();
    let invoking = StorageReadControl::with_limit(4096);
    assert!(matches!(
        graph.write_image(&mut output, &invoking),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        model.write_image(&mut output, &invoking),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(output.is_empty());
    drop((graph, model));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn committed_model_capture_and_native_publication_share_unrelated_weights() {
    let engine = Engine::new();
    let original = model(0.25);
    engine.save_model("large", &original).unwrap();
    let small = DeepModel {
        layers: vec![DeepLayerSpec::Input { dimensions: 1 }],
        alpha: 0.5,
        gating: GatingSpec::None,
    };
    for index in 0..64 {
        engine
            .save_model(&format!("small-{index:02}"), &small)
            .unwrap();
    }
    let large = std::sync::Arc::downgrade(&engine.durable.models.read()["large"]);
    let unread = std::sync::Arc::downgrade(&engine.durable.models.read()["small-00"]);
    let control = StorageReadControl::with_limit(8 << 20);
    let mut cut = None;
    let usage = allocation_counter::measure(|| {
        cut = Some(engine.capture_committed_memory(&control).unwrap());
    });
    assert!(usage.bytes_total < 1 << 19, "{usage:?}");
    let cut = cut.unwrap();
    let captured = cut.resources.models.captured(&cut.cut).unwrap();
    let saved = captured.get().unwrap().image("large").unwrap().unwrap();
    let retained = control.memory().used();
    let usage = allocation_counter::measure(|| engine.save_model("small-00", &small).unwrap());
    assert!(usage.bytes_total < 1 << 19, "{usage:?}");
    sql(&engine, "BEGIN; SAVEPOINT saved_models");
    engine.save_model("large", &small).unwrap();
    engine.save_model("private", &small).unwrap();
    sql(&engine, "ROLLBACK TO saved_models; COMMIT");
    assert_eq!(engine.load_model("large").unwrap(), Some(original));
    assert!(engine.load_model("private").unwrap().is_none());
    engine.save_model("later", &model(0.75)).unwrap();
    let expected = model_image(
        &engine.durable.models.read()["large"]
            .capture_image_with_control(&StorageReadControl::with_limit(2 << 20))
            .unwrap(),
    );
    drop(engine);
    assert_eq!(control.memory().used(), retained);
    assert_eq!(captured.get().unwrap().len(), 65);
    assert!(captured.get().unwrap().image("later").unwrap().is_none());
    assert_eq!(model_image(&saved), expected);
    drop((captured, cut));
    assert!(unread.upgrade().is_none());
    assert!(large.upgrade().is_some());
    assert!(control.memory().used() > 1 << 20);
    assert!(control.memory().used() < (1 << 20) + 4096);
    control.cancellation().cancel();
    assert!(saved
        .write_image(&mut Vec::new(), &StorageReadControl::with_limit(0))
        .is_err());
    drop(saved);
    assert!(large.upgrade().is_none());
    assert_eq!(control.memory().used(), 0);
}
