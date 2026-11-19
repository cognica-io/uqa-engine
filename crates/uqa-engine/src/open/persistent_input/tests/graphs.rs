//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_graph::{GraphStore, LabelKind};

fn reach_graphs(input: &mut PersistentEngineInput) {
    for _ in 0..65536 {
        if matches!(input.candidate.as_ref().unwrap().stage, Stage::Graphs(_)) {
            return;
        }
        assert!(input.advance(1).unwrap().is_none());
    }
    panic!("graph input was not reached")
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_graph_input_keeps_selected_labels_and_publishes_live_native_handles(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("graphs"));
    for name in ["g", "other"] {
        source.create_graph(name).unwrap();
    }
    source
        .create_graph_label("g", "node", LabelKind::Vertex)
        .unwrap();
    source
        .create_graph_label("g", "deleted", LabelKind::Edge)
        .unwrap();
    source.drop_graph_label("g", "deleted").unwrap();
    let expected = source
        .graph_with("g", |graph| graph.label_registry("g").unwrap())
        .unwrap()
        .unwrap();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    reach_graphs(&mut input);
    for _ in 0..8 {
        assert!(input.advance(1).unwrap().is_none());
    }
    source.drop_graph("g").unwrap();
    source.create_graph("later").unwrap();
    let session = finish(&mut input);
    {
        let graphs = session.engine().durable.graphs.read();
        assert_eq!(
            graphs.keys().map(String::as_str).collect::<Vec<_>>(),
            ["g", "other"]
        );
        assert_eq!(graphs["g"].label_registry("g").unwrap(), expected);
        assert!(matches!(
            graphs["g"].as_ref(),
            uqa_graph::GraphStoreHandle::Persistent(_)
        ));
    }
    let restored = session.finish().unwrap();
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    restored.create_graph("after").unwrap();
    restored
        .create_graph_label("after", "edge", LabelKind::Edge)
        .unwrap();
    restored
        .graph_with_mut("after", |graph| {
            let id = graph.allocate_vertex_id("node", "after")?;
            graph.add_vertex(uqa_core::Vertex::new(id, "node"), "after")
        })
        .unwrap();
    assert_eq!(
        restored
            .graph_with("after", |graph| graph
                .vertices_in_graph("after")
                .unwrap()
                .len())
            .unwrap(),
        Some(1)
    );
    drop((restored, input, source));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_graph_input_retires_partial_labels_and_the_private_transaction(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("graphs"));
    source.create_graph("g").unwrap();
    let registry = uqa_graph::GraphLabelRegistry {
        labels: (0..128)
            .map(|id| (format!("label-{id:04}"), id + 3))
            .collect(),
        ..uqa_graph::GraphLabelRegistry::default()
    };
    let catalog = source.storage.catalog.as_ref().unwrap();
    catalog
        .set_metadata(
            "graph_label_registry::g",
            &serde_json::to_string(&registry).unwrap(),
        )
        .unwrap();
    let before = catalog.metadata_with_prefix("graph_").unwrap();
    for terminal in ["cancel", "zero", "memory", "drop", "unwind"] {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&source, &control);
        reach_graphs(&mut input);
        for _ in 0..90 {
            assert!(input.advance(1).unwrap().is_none());
        }
        assert!(matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::Graphs(_)
        ));
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
        let occupied = if terminal == "memory" {
            Some(
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap(),
            )
        } else {
            None
        };
        match terminal {
            "cancel" => control.cancellation().cancel(),
            "unwind" => {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _input = input;
                    panic!("retire private graph restoration");
                }));
                assert!(!backend.in_transaction());
                assert_eq!(control.memory().used(), 0);
                continue;
            }
            _ => (),
        }
        if terminal != "drop" {
            let mut failed = false;
            for _ in 0..65536 {
                match input.advance(usize::from(terminal != "zero")) {
                    Err(_) => {
                        failed = true;
                        break;
                    }
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("terminal graph restoration completed"),
                }
            }
            assert!(failed && input.candidate.is_none());
            assert!(input.advance(1).is_err());
        }
        drop((input, occupied));
        assert!(!backend.in_transaction());
        assert_eq!(control.memory().used(), 0);
    }
    assert_eq!(catalog.metadata_with_prefix("graph_").unwrap(), before);
    source.create_graph("recovered").unwrap();
}
