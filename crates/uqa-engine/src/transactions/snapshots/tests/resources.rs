//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};
use crate::state::SnapshotCut;
use uqa_core::{Edge, Vertex};
use uqa_graph::{CapturedPath, CapturedPathIndex, GraphImageSnapshot};
use uqa_ml::{model_image::ModelImageSnapshot, DeepLayerSpec, DeepModel, GatingSpec};
use uqa_sql::catalog::graph_oids::GraphOidRegistry;
use uqa_storage::analyzer_image::{write_analyzer_image, AnalyzerImageSnapshot};

fn populate(engine: &Engine, value: f64) {
    let mut vertex = Vertex::new(1, "node");
    vertex
        .properties
        .insert("value".into(), Value::Float(value));
    vertex
        .properties
        .insert("payload".into(), Value::Str("g".repeat(65536)));
    engine.add_graph_vertex(vertex, "g").unwrap();
    engine
        .add_graph_vertex(Vertex::new(2, "node"), "g")
        .unwrap();
    engine
        .add_graph_edge(Edge::new(1, 1, 2, "edge"), "g")
        .unwrap();
    engine
        .build_path_index("saved", "g", &[vec!["edge".into()]])
        .unwrap();
    engine
        .save_model(
            "saved",
            &DeepModel {
                layers: vec![DeepLayerSpec::Embed {
                    embedding: vec![value; 65536],
                }],
                alpha: value,
                gating: GatingSpec::None,
            },
        )
        .unwrap();
    engine
        .save_scoring_params("saved", &format!("{{\"alpha\":{value}}}"))
        .unwrap();
}

fn graph_image(value: &GraphImageSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    value
        .write_image(&mut bytes, &StorageReadControl::with_limit(4096))
        .unwrap();
    bytes
}

fn model_image(value: &ModelImageSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    value
        .write_image(&mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    bytes
}

fn analyzer_image(value: &AnalyzerImageSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    value
        .write_image(&mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    bytes
}

fn original_images(engine: &Engine) -> [Vec<u8>; 3] {
    let control = StorageReadControl::with_limit(2 << 20);
    let graph = engine.durable.graphs.read()["g"]
        .capture_image_with_control(&control)
        .unwrap();
    let model = engine.durable.models.read()["saved"]
        .capture_image_with_control(&control)
        .unwrap();
    let mut analyzer = Vec::new();
    write_analyzer_image(
        &engine.durable.named_analyzers.read()["saved"],
        &mut analyzer,
        &StorageReadControl::with_limit(0),
    )
    .unwrap();
    [graph_image(&graph), model_image(&model), analyzer]
}

fn same_metadata(selected: &SnapshotCut, original_oids: &GraphOidRegistry) {
    let paths = selected
        .resources
        .path_indexes
        .captured(&selected.cut)
        .unwrap();
    assert_eq!(
        **paths.get().unwrap().get("g::saved").unwrap(),
        CapturedPathIndex {
            paths: vec![CapturedPath {
                sequence: vec!["edge".into()],
                pairs: vec![(1, 2)]
            }],
        }
    );
    let scoring = selected
        .resources
        .scoring_params
        .captured(&selected.cut)
        .unwrap();
    assert_eq!(scoring.get().unwrap().iter().count(), 1);
    assert_eq!(
        scoring.get().unwrap().get("saved").map(String::as_str),
        Some("{\"alpha\":0.25}")
    );
    let oids = selected
        .catalogs
        .graph_catalog_oids
        .captured(&selected.cut)
        .unwrap();
    assert!(!original_oids.is_empty());
    assert_eq!(oids.get().unwrap().len(), original_oids.len());
    for ((name, value), (old_name, old)) in oids.get().unwrap().iter().zip(original_oids.iter()) {
        assert_eq!(name, old_name);
        assert_eq!(value.namespace, old.namespace);
        assert_eq!(value.label_sequence, old.label_sequence);
        assert!(!old.labels.is_empty());
        assert_eq!(value.labels.len(), old.labels.len());
        for ((id, label), (old_id, old_label)) in value.labels.iter().zip(old.labels.iter()) {
            assert_eq!(id, old_id);
            assert_eq!(label.relation, old_label.relation);
            assert_eq!(label.sequence, old_label.sequence);
            assert_eq!(label.id_default, old_label.id_default);
            assert_eq!(label.properties_default, old_label.properties_default);
            assert!(label.not_null.iter().eq(old_label.not_null.iter()));
            assert_eq!(label.toast_table, old_label.toast_table);
            assert_eq!(label.toast_index, old_label.toast_index);
            assert_eq!(label.primary_key, old_label.primary_key);
            assert_eq!(label.endpoint_indexes, old_label.endpoint_indexes);
            assert_eq!(label.trigger, old_label.trigger);
        }
    }
}

#[rstest::rstest]
#[case("COMMIT")]
#[case("ROLLBACK")]
fn transaction_resource_sources_preserve_original_images_across_private_mutation_and_closure(
    #[case] completion: &str,
) {
    let engine = Engine::new();
    sql(
        &engine,
        "CREATE TABLE docs(id integer); INSERT INTO docs VALUES(1)",
    );
    engine.create_graph("g").unwrap();
    populate(&engine, 0.25);
    engine
        .register_named_analyzer("saved", r#"{"tokenizer":{"type":"whitespace"}}"#)
        .unwrap();
    let original = original_images(&engine);
    let original_oids = engine.durable.graph_catalog_oids.snapshot();
    let root = Arc::downgrade(&engine.durable.graphs.snapshot());
    sql(&engine, "BEGIN; UPDATE docs SET id=2");
    populate(&engine, 0.5);
    engine
        .register_named_analyzer("saved", r#"{"tokenizer":{"type":"keyword"}}"#)
        .unwrap();
    sql(&engine, "SAVEPOINT nested");
    engine.drop_graph("g").unwrap();
    sql(&engine, "ROLLBACK TO nested");
    let control = StorageReadControl::with_limit(2 << 20);
    let selected = capture(&engine, &control);
    let small = StorageReadControl::with_limit(16 << 10);
    let denied = capture(&engine, &small);
    assert!(selected
        .resources
        .graphs
        .retained(&selected.cut)
        .unwrap()
        .is_none());
    assert!(selected
        .resources
        .models
        .retained(&selected.cut)
        .unwrap()
        .is_none());
    sql(&engine, completion);
    assert!(denied.cut.check().is_err());
    populate(&engine, 0.75);
    assert_ne!(original_images(&engine)[0], original[0]);
    drop(engine);
    assert!(root.upgrade().is_none());
    let graphs = selected.resources.graphs.captured(&selected.cut).unwrap();
    let models = selected.resources.models.captured(&selected.cut).unwrap();
    let analyzers = selected
        .resources
        .named_analyzers
        .captured(&selected.cut)
        .unwrap();
    let graph = graphs.get().unwrap().get("g").unwrap().clone();
    let model = models.get().unwrap().image("saved").unwrap().unwrap();
    let analyzer = analyzers.get().unwrap().image("saved").unwrap().unwrap();
    assert_eq!(
        [
            graph_image(&graph),
            model_image(&model),
            analyzer_image(&analyzer)
        ],
        original
    );
    same_metadata(&selected, &original_oids);
    assert_eq!(
        selected
            .table(&RelationIdentity::new("public", "docs"))
            .unwrap()
            .document_store
            .get_field(1, "id")
            .unwrap(),
        Some(Value::Int(1))
    );
    drop((denied, selected, graphs, models, analyzers));
    assert_eq!(small.memory().used(), 0);
    control.cancellation().cancel();
    let mut output = Vec::new();
    let invoking = StorageReadControl::with_limit(4096);
    assert!(graph.write_image(&mut output, &invoking).is_err());
    assert!(model.write_image(&mut output, &invoking).is_err());
    assert!(analyzer.write_image(&mut output, &invoking).is_err());
    assert!(output.is_empty());
    drop((graph, model, analyzer));
    assert_eq!(control.memory().used(), 0);
}
