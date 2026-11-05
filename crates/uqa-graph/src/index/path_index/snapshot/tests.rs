//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{GraphStore, MemoryGraphStore};
use uqa_core::{Edge, Vertex};

#[test]
fn native_path_image_admits_actual_immutable_capacity_without_copying_or_replaying_paths() {
    let mut graph = MemoryGraphStore::new();
    graph.create_graph("g").unwrap();
    for id in 0..=128 {
        graph.add_vertex(Vertex::new(id, "node"), "g").unwrap();
        if id > 0 {
            graph
                .add_edge(Edge::new(id, id - 1, id, "edge"), "g")
                .unwrap();
        }
    }
    let sequences = [
        vec!["edge".into()],
        vec![],
        vec!["edge".into(), "edge".into()],
        vec!["a/b".into()],
        vec!["a".into(), "b".into()],
        vec!["long".repeat(8192)],
        vec!["edge".into()],
    ];
    drop(PathIndex::build(&graph, "g", &sequences).unwrap());
    let mut source = None;
    let allocated = allocation_counter::measure(|| {
        source = Some(PathIndex::build(&graph, "g", &sequences).unwrap());
    });
    let source = source.unwrap();
    let paths = source.memory_paths().unwrap();
    // The owner measures all live payload allocations; Arc's two reference counters remain outside payload accounting.
    assert_eq!(
        allocated.bytes_current as usize,
        paths.retained_bytes() + 2 * size_of::<usize>()
    );
    let footprint = paths.retained_bytes() + PathIndexImageSnapshot::wrapper_bytes();
    let small = StorageReadControl::with_limit(footprint - 1);
    let measured = allocation_counter::measure(|| {
        assert!(matches!(
            source.capture_native_memory_with_control(&small),
            Err(StorageBackendError::Memory(_))
        ));
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(small.memory().used(), 0);
    let control = StorageReadControl::with_limit(footprint);
    let mut image = None;
    let measured = allocation_counter::measure(|| {
        image = Some(source.capture_native_memory_with_control(&control).unwrap());
    });
    assert_eq!(measured.count_total, 1);
    let image = image.unwrap();
    assert_eq!(control.memory().used(), footprint);
    assert!(std::ptr::eq(image.get().unwrap(), &raw const paths.image));
    assert_eq!(image.get().unwrap().paths.len(), 6);
    assert_eq!(paths.get(&["edge".into()]).unwrap().len(), 128);
    assert_eq!(
        paths.get(&["edge".into(), "edge".into()]).unwrap().len(),
        127
    );
    assert_eq!(paths.get(&[]).unwrap().len(), 129);
    drop((source, graph));
    assert_eq!(image.get().unwrap().paths.len(), 6);
    control.cancellation().cancel();
    assert!(matches!(
        image.get(),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop(image);
    assert_eq!(control.memory().used(), 0);
}
