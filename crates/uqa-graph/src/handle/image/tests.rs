//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::GraphStore;
use uqa_core::{Edge, Value, Vertex};

fn source() -> Arc<GraphStoreHandle> {
    let mut source = GraphStoreHandle::default();
    source.create_graph("g").unwrap();
    let mut vertex = Vertex::new(1, "original");
    vertex
        .properties
        .insert("payload".into(), Value::Str("x".repeat(1 << 20)));
    source.add_vertex(vertex, "g").unwrap();
    source.add_vertex(Vertex::new(2, "other"), "g").unwrap();
    source.add_edge(Edge::new(3, 1, 2, "link"), "g").unwrap();
    Arc::new(source)
}

fn encode(source: &GraphImageSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    source
        .write_image(&mut bytes, &StorageReadControl::with_limit(4096))
        .unwrap();
    bytes
}

#[test]
fn captured_graph_images_keep_immutable_sources_and_release_their_complete_allowance() {
    let mut source = source();
    let original = source.clone();
    let first = StorageReadControl::with_limit(4 << 20);
    let second = StorageReadControl::with_limit(4 << 20);
    let captured = source.capture_image_with_control(&first).unwrap();
    let independent = source.capture_image_with_control(&second).unwrap();
    assert!(Arc::ptr_eq(&source, &captured.0.source));
    assert!(first.memory().used() >= 1 << 20);
    assert_eq!(first.memory().used(), second.memory().used());
    let expected = encode(&captured);
    Arc::make_mut(&mut source)
        .add_vertex(Vertex::new(4, "later"), "g")
        .unwrap();
    assert!(!Arc::ptr_eq(&source, &original));
    drop((source, original));
    let retained = captured.clone();
    drop(captured);
    assert_eq!(encode(&retained), expected);
    drop(independent);
    assert_eq!(second.memory().used(), 0);
    assert!(first.memory().used() >= 1 << 20);
    drop(retained);
    assert_eq!(first.memory().used(), 0);
}

#[test]
fn graph_capture_rejects_insufficient_payload_allowance_without_retaining_source_records() {
    let mut graph = GraphStoreHandle::default();
    graph.create_graph("g").unwrap();
    let mut vertex = Vertex::new(1, "label");
    let value = "x".repeat(1 << 20);
    vertex
        .properties
        .insert("payload".into(), Value::Str(value));
    graph.add_vertex(vertex, "g").unwrap();
    let source = Arc::new(graph);
    let small = StorageReadControl::with_limit(4096);
    assert!(matches!(
        source.capture_image_with_control(&small),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(Arc::strong_count(&source), 1);
    let control = StorageReadControl::with_limit(2 << 20);
    let captured = source.capture_image_with_control(&control).unwrap();
    assert!(control.memory().used() >= 1 << 20);
    assert!(Arc::ptr_eq(&source, &captured.0.source));
    drop(captured);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(source.vertex_ids_in_graph("g").unwrap().len(), 1);
}

#[test]
fn captured_graph_output_checks_original_and_invoking_cancellation() {
    struct CancelAfterWrite<'a> {
        calls: usize,
        control: &'a StorageReadControl,
    }
    impl std::io::Write for CancelAfterWrite<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            self.control.cancellation().cancel();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let source = source();
    let original = StorageReadControl::with_limit(4 << 20);
    let captured = source.capture_image_with_control(&original).unwrap();
    let cancelled = StorageReadControl::with_limit(4096);
    cancelled.cancellation().cancel();
    let mut bytes = Vec::new();
    assert!(matches!(
        captured.write_image(&mut bytes, &cancelled),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(bytes.is_empty());
    let mut output = CancelAfterWrite {
        calls: 0,
        control: &original,
    };
    assert!(matches!(
        captured.write_image(&mut output, &StorageReadControl::with_limit(4096)),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(output.calls, 1);
    assert!(matches!(
        captured.write_image(&mut bytes, &StorageReadControl::with_limit(4096)),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop(captured);
    assert_eq!(original.memory().used(), 0);
}
