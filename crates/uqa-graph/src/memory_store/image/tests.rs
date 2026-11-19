//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod input;
mod registration;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod repository;
mod stream;
use crate::{store::GraphStore, types::Direction};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use uqa_core::{memory::MemoryError, DatumValue, RecordValue, Value};

fn encode(store: &MemoryGraphStore) -> Vec<u8> {
    let control = StorageReadControl::with_limit(0);
    let mut bytes = Vec::new();
    assert_eq!(
        store.write_image(&mut bytes, &control).unwrap(),
        bytes.len() as u64
    );
    assert_eq!(control.memory().used(), 0);
    bytes
}

fn decode(bytes: &[u8], control: &StorageReadControl) -> MemoryGraphStore {
    MemoryGraphStore::read_image(&mut &bytes[..], control).unwrap()
}

// Independent byte fixture: two empty-property vertices, one edge, one partition and no label registry. No graph/image encoder constructs this expectation.
fn fixture(ids: [u64; 2], members: [u64; 2], edge_member: u64, next_vertex: u64) -> Vec<u8> {
    fn number(bytes: &mut Vec<u8>, n: u64) {
        bytes.extend_from_slice(&n.to_le_bytes());
    }
    let mut bytes = b"UQAGRA01".to_vec();
    for n in [next_vertex, 5, 2] {
        number(&mut bytes, n);
    }
    for id in ids {
        number(&mut bytes, id);
        number(&mut bytes, 1);
        bytes.push(b'p');
        bytes.extend_from_slice(b"UQAVAL01");
        bytes.push(17);
        number(&mut bytes, 0);
    }
    for n in [1, 4, 1, 2, 1] {
        number(&mut bytes, n);
    }
    bytes.push(b'r');
    bytes.extend_from_slice(b"UQAVAL01");
    bytes.push(17);
    for n in [0, 1, 1] {
        number(&mut bytes, n);
    }
    bytes.push(b'g');
    for n in [2, members[0], members[1], 1, edge_member, 0] {
        number(&mut bytes, n);
    }
    bytes
}

fn edge_ids(store: &MemoryGraphStore, graph: &str) -> BTreeSet<u64> {
    store
        .edges_in_graph(graph)
        .unwrap()
        .into_iter()
        .map(|edge| edge.edge_id)
        .collect()
}

fn source() -> MemoryGraphStore {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g").unwrap();
    store.add_vertex(Vertex::new(1, "p"), "g").unwrap();
    store.add_vertex(Vertex::new(2, "p"), "g").unwrap();
    store.add_edge(Edge::new(4, 1, 2, "r"), "g").unwrap();
    store
}

#[test]
fn memory_graph_images_match_independent_wire_and_empty_allocation_fixtures() {
    let bytes = fixture([1, 2], [1, 2], 4, 3);
    assert_eq!(encode(&source()), bytes);
    let control = StorageReadControl::with_limit(1 << 20);
    let restored = decode(&bytes, &control);
    assert_eq!(
        restored
            .neighbors(1, Some("r"), Direction::Out, "g")
            .unwrap(),
        [2]
    );
    assert_eq!(
        restored.vertex_ids_in_graph("g").unwrap(),
        BTreeSet::from([1, 2])
    );
    assert_eq!(edge_ids(&restored, "g"), BTreeSet::from([4]));
    assert_eq!(encode(&restored), bytes);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
    for source in [MemoryGraphStore::new(), MemoryGraphStore::default()] {
        let mut expected = b"UQAGRA01".to_vec();
        for n in [source.next_vertex_id, source.next_edge_id, 0, 0, 0, 0] {
            expected.extend_from_slice(&n.to_le_bytes());
        }
        assert_eq!(encode(&source), expected);
        let restored = decode(&expected, &control);
        let mut restored = restored;
        assert_eq!(restored.next_vertex_id().unwrap(), source.next_vertex_id);
        assert_eq!(restored.next_edge_id().unwrap(), source.next_edge_id);
        drop(restored);
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_graph_images_preserve_evaluated_properties_memberships_and_algebra() {
    let mut source = source();
    let mut vertex = Vertex::new(1, "π");
    vertex.properties = BTreeMap::from([
        ("void".into(), Value::Void),
        ("null".into(), Value::Null),
        (
            "nan".into(),
            Value::Float(f64::from_bits(0xfff8_0000_0000_0042)),
        ),
        ("zero".into(), Value::Float(-0.0)),
        (
            "lazy".into(),
            Value::Datum(DatumValue::new(25, u32::MAX, vec![0, 255])),
        ),
        (
            "record".into(),
            Value::Record(RecordValue::from_parts(
                vec![("x".into(), Value::Void), ("x".into(), Value::Null)],
                Some(400),
            )),
        ),
        ("$uqa_type".into(), Value::Str("datum".into())),
    ]);
    source.add_vertex(vertex, "g").unwrap();
    source.add_edge(Edge::new(5, 1, 1, "loop"), "g").unwrap();
    source.add_edge(Edge::new(6, 1, 2, "r"), "g").unwrap();
    source.copy_graph("g", "copy").unwrap();
    source.create_graph("empty").unwrap();
    source.create_graph("only_vertices").unwrap();
    source
        .add_vertex(Vertex::new(2, "p"), "only_vertices")
        .unwrap();
    source.union_graphs("g", "only_vertices", "union").unwrap();
    source
        .intersect_graphs("g", "only_vertices", "intersection")
        .unwrap();
    source
        .difference_graphs("g", "only_vertices", "difference")
        .unwrap();
    source
        .insert_raw_vertex(Vertex::new(99, "unattached"))
        .unwrap();
    source
        .insert_raw_edge(Edge::new(100, 400, 500, "unattached"))
        .unwrap();
    let bytes = encode(&source);
    let expected_names = source.graph_names();
    let control = StorageReadControl::with_limit(1 << 20);
    let restored = decode(&bytes, &control);
    assert_eq!(restored.graph_names(), expected_names);
    assert_eq!(
        restored.vertex_ids_in_graph("difference").unwrap(),
        BTreeSet::from([1])
    );
    assert_eq!(
        restored.graphs.get("difference").unwrap().edge_ids.to_set(),
        BTreeSet::from([4, 5, 6])
    );
    assert!(restored.vertex_graphs(99).is_empty());
    assert_eq!(restored.get_edge(100).unwrap().target_id, 500);
    for graph in expected_names {
        assert_eq!(
            source.vertex_ids_in_graph(&graph).unwrap(),
            restored.vertex_ids_in_graph(&graph).unwrap()
        );
        assert_eq!(
            source.edges_in_graph(&graph),
            restored.edges_in_graph(&graph)
        );
        assert_eq!(
            source.vertex_label_counts(&graph).unwrap(),
            restored.vertex_label_counts(&graph).unwrap()
        );
        assert_eq!(
            source.degree_distribution(&graph),
            restored.degree_distribution(&graph)
        );
    }
    drop(source);
    assert_eq!(
        restored.out_edge_ids(1, "g").unwrap(),
        BTreeSet::from([4, 5, 6])
    );
    assert_eq!(restored.in_edge_ids(1, "g").unwrap(), BTreeSet::from([5]));
    let props = &restored.get_vertex(1).unwrap().properties;
    assert!(matches!(props["nan"], Value::Float(v) if v.to_bits() == 0xfff8_0000_0000_0042));
    assert!(matches!(props["zero"], Value::Float(v) if v.to_bits() == (-0.0_f64).to_bits()));
    assert!(matches!(&props["lazy"], Value::Datum(v) if v.offset() == u32::MAX));
    assert_eq!(encode(&restored), bytes);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_graph_images_preserve_unused_ids_registry_merges_and_future_allocation() {
    let mut source = source();
    for _ in 0..9 {
        source.next_vertex_id().unwrap();
        source.next_edge_id().unwrap();
    }
    for _ in 0..5 {
        source.allocate_vertex_id("unused", "g").unwrap();
    }
    let mut first = GraphLabelRegistry::default();
    first.register_label("first", LabelKind::Vertex).unwrap();
    first.next_sequence(3).unwrap();
    let mut second = GraphLabelRegistry::default();
    second.register_label("alias", LabelKind::Edge).unwrap();
    second.next_sequence(3).unwrap();
    second.next_sequence(3).unwrap();
    source.import_label_registry("future", &first).unwrap();
    source.import_label_registry("future", &second).unwrap();
    // Import merging preserves label aliases and tombstones even where IDs overlap; image decoding must not invent a registry migration.
    second.remove_label("alias");
    source.import_label_registry("future", &second).unwrap();
    let mut registry = source.native_registry("future");
    registry.allocation_id = [19; 16];
    registry.kinds.remove("first").unwrap();
    let expected_registry = serde_json::to_value(registry.to_record()).unwrap();
    source.put_registry("future", registry).unwrap();
    let bytes = encode(&source);
    let control = StorageReadControl::with_limit(1 << 20);
    let restored = decode(&bytes, &control);
    assert_eq!(
        serde_json::to_value(restored.label_registry("future")).unwrap(),
        expected_registry
    );
    let mut restored = restored;
    for _ in 0..3 {
        assert_eq!(
            restored.next_vertex_id().unwrap(),
            source.next_vertex_id().unwrap()
        );
        assert_eq!(
            restored.next_edge_id().unwrap(),
            source.next_edge_id().unwrap()
        );
        assert_eq!(
            restored.allocate_vertex_id("unused", "g").unwrap(),
            source.allocate_vertex_id("unused", "g").unwrap()
        );
    }
    for store in [&mut source, &mut restored] {
        store.create_graph("future").unwrap();
    }
    assert_eq!(
        restored.allocate_vertex_id("first", "future"),
        source.allocate_vertex_id("first", "future")
    );
    assert_eq!(
        restored.allocate_edge_id("alias", "future"),
        source.allocate_edge_id("alias", "future")
    );
    assert_eq!(encode(&restored), encode(&source));
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_graph_images_keep_label_drop_edges_and_empty_index_observations() {
    let mut source = MemoryGraphStore::new();
    source.create_graph("g").unwrap();
    let a = source.allocate_vertex_id("old", "g").unwrap();
    let b = source.allocate_vertex_id("keep", "g").unwrap();
    let edge = source.allocate_edge_id("link", "g").unwrap();
    source.add_vertex(Vertex::new(a, "old"), "g").unwrap();
    source.add_vertex(Vertex::new(b, "keep"), "g").unwrap();
    source.add_edge(Edge::new(edge, a, b, "link"), "g").unwrap();
    source.drop_label("g", "old").unwrap();
    let bytes = encode(&source);
    let control = StorageReadControl::with_limit(1 << 20);
    let restored = decode(&bytes, &control);
    assert!(restored.get_vertex(a).is_none());
    assert_eq!(restored.get_edge(edge).unwrap().source_id, a);
    assert_eq!(
        restored.vertex_label_counts("g").unwrap(),
        BTreeMap::from([("keep".into(), 1)])
    );
    assert_eq!(
        restored.vertex_ids_by_label("old", "g").unwrap(),
        Vec::<u64>::new()
    );
    assert_eq!(
        restored.label_registry("g").dropped_label_ids,
        BTreeSet::from([3])
    );
    assert_eq!(encode(&restored), bytes);
    let mut restored = restored;
    assert_eq!(
        restored.allocate_vertex_id("old", "g").unwrap(),
        source.allocate_vertex_id("old", "g").unwrap()
    );
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_graph_images_reject_truncation_order_membership_and_counter_corruption() {
    let bytes = fixture([1, 2], [1, 2], 4, 3);
    let control = StorageReadControl::with_limit(1 << 20);
    for n in 0..bytes.len() {
        assert!(
            MemoryGraphStore::read_image(&mut &bytes[..n], &control).is_err(),
            "prefix {n}"
        );
        assert_eq!(control.memory().used(), 0);
    }
    let mut invalid = vec![
        fixture([1, 1], [1, 2], 4, 3),
        fixture([2, 1], [1, 2], 4, 3),
        fixture([1, 2], [1, 1], 4, 3),
        fixture([1, 2], [1, 3], 4, 3),
        fixture([1, 2], [1, 2], 9, 3),
        fixture([1, 2], [1, 2], 4, 2),
    ];
    let mut extra = bytes.clone();
    extra.push(0);
    invalid.push(extra);
    let mut version = bytes.clone();
    version[7] = b'2';
    invalid.push(version);
    let mut utf8 = bytes.clone();
    utf8[48] = 255;
    invalid.push(utf8);
    let mut not_map = bytes.clone();
    not_map[57] = 0;
    invalid.push(not_map);
    for bad in invalid {
        assert!(MemoryGraphStore::read_image(&mut bad.as_slice(), &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    assert_eq!(encode(&decode(&bytes, &control)), bytes);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_graph_images_reject_corrupt_derived_sources_before_output() {
    let original = source();
    for case in 0..8 {
        let mut broken = original.clone();
        match case {
            0 => {
                broken.vertices.remove(&1).unwrap();
            }
            1 => {
                let mut edge = broken.edges.get(&4).unwrap().clone();
                edge.edge_id = 9;
                let bytes = super::super::publication::entity_bytes(
                    &edge.label,
                    &edge.properties,
                    broken.edges.budget(),
                )
                .unwrap();
                broken.edges.insert(4, edge, bytes, bytes).unwrap();
            }
            2 => {
                broken
                    .vertex_membership
                    .insert(
                        1,
                        super::super::NativeSet::new(broken.vertex_membership.budget()),
                        0,
                        0,
                    )
                    .unwrap();
            }
            3 => {
                super::super::publication::membership(
                    &mut broken.edge_membership,
                    4,
                    "unknown",
                    true,
                )
                .unwrap();
            }
            4..=7 => {
                let mut partition = broken.partition_candidate("g").unwrap();
                match case {
                    4 => partition.adj_out.clear(),
                    5 => {
                        let mut ids = super::super::NativeSet::new(partition.adj_in.budget());
                        ids.insert(4, 0).unwrap();
                        let bytes = ids.retained_bytes().unwrap();
                        partition.adj_in.insert(999, ids, 0, bytes).unwrap();
                    }
                    6 => partition.label_index.clear(),
                    7 => {
                        let mut ids =
                            super::super::NativeSet::new(partition.vertex_label_index.budget());
                        ids.insert(1, 0).unwrap();
                        let bytes = ids.retained_bytes().unwrap();
                        partition
                            .vertex_label_index
                            .insert_named("wrong", ids, 0, bytes)
                            .unwrap();
                    }
                    _ => unreachable!(),
                }
                broken.put_partition("g", partition).unwrap();
            }
            _ => unreachable!(),
        }
        let mut output = Vec::new();
        assert!(
            broken
                .write_image(&mut output, &StorageReadControl::with_limit(1 << 20))
                .is_err(),
            "case {case}"
        );
        assert!(output.is_empty());
    }
}

#[test]
fn memory_graph_images_admit_private_candidates_and_release_every_failure() {
    let bytes = fixture([1, 2], [1, 2], 4, 3);
    let unlimited = StorageReadControl::with_limit(1 << 20);
    let image = decode(&bytes, &unlimited);
    let retained = unlimited.memory().used();
    let required = unlimited.memory().peak();
    assert!(retained > 0);
    assert!(required >= retained);
    drop(image);
    assert_eq!(unlimited.memory().used(), 0);
    for limit in 0..required {
        let control = StorageReadControl::with_limit(limit);
        assert!(matches!(
            MemoryGraphStore::read_image(&mut bytes.as_slice(), &control),
            Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
        ));
        assert_eq!(control.memory().used(), 0);
        assert!(control.memory().peak() <= limit);
    }
    let exact = StorageReadControl::with_limit(required);
    let image = decode(&bytes, &exact);
    assert_eq!(exact.memory().used(), retained);
    let saved = image.clone();
    drop(image);
    assert_eq!(exact.memory().used(), retained);
    assert_eq!(encode(&saved), bytes);
    drop(saved);
    assert_eq!(exact.memory().used(), 0);
    let mut huge = b"UQAGRA01".to_vec();
    for n in [1_u64, 1, u64::MAX] {
        huge.extend_from_slice(&n.to_le_bytes());
    }
    assert!(matches!(
        MemoryGraphStore::read_image(&mut huge.as_slice(), &exact),
        Err(StorageBackendError::Memory(MemoryError::SizeOverflow))
    ));
    assert_eq!(exact.memory().used(), 0);
}

#[test]
fn memory_graph_images_stream_large_borrowed_properties_with_bounded_io() {
    #[derive(Default)]
    struct Sink {
        written: usize,
        largest: usize,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.largest = self.largest.max(bytes.len());
            self.written += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut source = source();
    let mut vertex = source.get_vertex(1).unwrap().clone();
    vertex
        .properties
        .insert("blob".into(), Value::Bytes(vec![37; 2 << 20]));
    source.insert_raw_vertex(vertex).unwrap();
    let control = StorageReadControl::with_limit(1);
    let mut sink = Sink::default();
    assert_eq!(
        source.write_image(&mut sink, &control).unwrap(),
        sink.written as u64
    );
    assert!(sink.written > 2 << 20);
    assert!(sink.largest <= 16 << 10);
    assert_eq!(control.memory().peak(), 0);
    let bytes = encode(&source);
    let small = StorageReadControl::with_limit(64 << 10);
    assert!(matches!(
        MemoryGraphStore::read_image(&mut bytes.as_slice(), &small),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(small.memory().used(), 0);
    let enough = StorageReadControl::with_limit(3 << 20);
    let image = decode(&bytes, &enough);
    assert_eq!(encode(&image), bytes);
    drop(image);
    assert_eq!(enough.memory().used(), 0);
}

#[test]
fn memory_graph_images_keep_typed_cancellation_between_io_attempts() {
    struct Interrupted {
        token: uqa_core::CancellationToken,
        calls: usize,
        partial: bool,
    }
    impl Read for Interrupted {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            self.token.cancel();
            if self.partial {
                bytes[0] = b'U';
                Ok(1)
            } else {
                Err(std::io::ErrorKind::Interrupted.into())
            }
        }
    }
    impl Write for Interrupted {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            self.token.cancel();
            if self.partial {
                Ok(1)
            } else {
                Err(std::io::ErrorKind::Interrupted.into())
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for partial in [false, true] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut io = Interrupted {
            token: control.cancellation().clone(),
            calls: 0,
            partial,
        };
        assert!(matches!(
            MemoryGraphStore::read_image(&mut io, &control),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(io.calls, 1);
        control.cancellation().reset();
        io.calls = 0;
        assert!(matches!(
            source().write_image(&mut io, &control),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(io.calls, 1);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn memory_graph_images_propagate_partial_io_failures_without_publishing_a_candidate() {
    struct Failing {
        bytes: Vec<u8>,
        allowance: usize,
    }
    impl Write for Failing {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let count = bytes.len().min(self.allowance);
            if count == 0 {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            self.bytes.extend_from_slice(&bytes[..count]);
            self.allowance -= count;
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Failing {
        bytes: Vec::new(),
        allowance: 79,
    };
    let control = StorageReadControl::with_limit(1 << 20);
    assert!(source().write_image(&mut sink, &control).is_err());
    assert_eq!(sink.bytes.len(), 79);
    assert_eq!(sink.bytes, fixture([1, 2], [1, 2], 4, 3)[..79]);
    assert!(MemoryGraphStore::read_image(&mut sink.bytes.as_slice(), &control).is_err());
    assert_eq!(control.memory().used(), 0);
    assert_eq!(encode(&source()), fixture([1, 2], [1, 2], 4, 3));
}
