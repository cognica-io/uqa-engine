//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn read(store: &MemoryGraphStore, control: &StorageReadControl, window: usize) -> Vec<u8> {
    let mut reader = store.image_reader(control).unwrap();
    let mut bytes = Vec::new();
    let mut output = vec![0; window];
    while !reader.prepare(3).unwrap().ready {}
    loop {
        let count = reader.read(&mut output).unwrap();
        assert!(count <= window.min(16384));
        if count == 0 {
            return bytes;
        }
        bytes.extend_from_slice(&output[..count]);
    }
}

#[test]
fn graph_image_readers_preserve_independent_wire_and_allocation_state() {
    let store = source();
    let control = StorageReadControl::with_limit(256 << 10);
    for window in [1, 7, 32768] {
        assert_eq!(
            read(&store, &control, window),
            fixture([1, 2], [1, 2], 4, 3)
        );
        assert_eq!(control.memory().used(), 0);
    }
    let mut store = source();
    let mut registry = GraphLabelRegistry::default();
    registry.register_label("later", LabelKind::Vertex).unwrap();
    registry.next_sequence(3).unwrap();
    registry.remove_label("later");
    store.import_label_registry("g", &registry).unwrap();
    store.next_vertex_id().unwrap();
    store.next_edge_id().unwrap();
    let bytes = read(&store, &control, 1);
    let destination = StorageReadControl::with_limit(2 << 20);
    let mut restored = decode(&bytes, &destination);
    assert_eq!(read(&restored, &control, 17), bytes);
    assert_eq!(
        restored.next_vertex_id().unwrap(),
        store.next_vertex_id().unwrap()
    );
    assert_eq!(
        restored.next_edge_id().unwrap(),
        store.next_edge_id().unwrap()
    );
    assert_eq!(
        serde_json::to_value(restored.label_registry("g")).unwrap(),
        serde_json::to_value(store.label_registry("g")).unwrap()
    );
    drop(restored);
    assert_eq!(destination.memory().used(), 0);
}

#[test]
fn graph_image_preparation_bounds_primary_derived_and_empty_index_positions() {
    let mut store = source();
    for id in 10..510 {
        store
            .add_vertex(Vertex::new(id, format!("label{id}")), "g")
            .unwrap();
        store.remove_vertex(id, "g").unwrap();
    }
    let control = StorageReadControl::with_limit(256 << 10);
    let mut cursor = validation::Validation::new(&store);
    let mut done = false;
    let measured = allocation_counter::measure(|| {
        while !done {
            let before = cursor.visited;
            let progress = cursor.advance(1, &control).unwrap();
            assert_eq!(progress.positions, 1);
            done = progress.ready;
            assert_eq!(cursor.visited - before, 1);
        }
    });
    assert_eq!(measured.bytes_total, 0, "{measured:?}");
    assert!(
        cursor.visited > 1000,
        "empty derived entries must consume bounded positions"
    );
    let mut reader = store.image_reader(&control).unwrap();
    for _ in 0..100 {
        assert!(!reader.prepare(1).unwrap().ready);
    }
    assert_eq!(reader.read(&mut []).unwrap(), 0);
    assert!(!reader.prepare(1).unwrap().ready);
    drop(reader);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn graph_image_readers_borrow_large_properties_under_constant_admission() {
    let mut store = source();
    let control = StorageReadControl::with_limit(256 << 10);
    let reader = store.image_reader(&control).unwrap();
    let required = control.memory().used();
    assert!(required > 0 && required < (128 << 10));
    drop(reader);
    let mut vertex = store.get_vertex(1).unwrap().clone();
    vertex
        .properties
        .insert("large".into(), Value::Bytes(vec![37; (2 << 20) + 31]));
    store.insert_raw_vertex(vertex).unwrap();
    let expected = encode(&store);
    let mut actual = vec![0; expected.len()];
    let exact = StorageReadControl::with_limit(required);
    let mut reader = store.image_reader(&exact).unwrap();
    let measured = allocation_counter::measure(|| {
        while !reader.prepare(1).unwrap().ready {}
        let mut position = 0;
        while position < actual.len() {
            let count = reader.read(&mut actual[position..]).unwrap();
            assert!(count > 0 && count <= 16384);
            position += count;
        }
        assert_eq!(reader.read(&mut [0; 1]).unwrap(), 0);
    });
    assert_eq!(measured.bytes_total, 0, "{measured:?}");
    assert_eq!(actual, expected);
    assert_eq!(exact.memory().used(), 0);
    exact.cancellation().cancel();
    assert!(reader.prepare(1).unwrap().ready);
    assert_eq!(reader.read(&mut [0; 1]).unwrap(), 0);
    let small = StorageReadControl::with_limit(required - 1);
    let measured = allocation_counter::measure(|| {
        assert!(matches!(
            store.image_reader(&small),
            Err(StorageBackendError::Memory(_))
        ));
    });
    assert_eq!(measured.bytes_total, 0);
    assert_eq!(small.memory().used(), 0);
}

#[test]
fn graph_image_readers_reject_preparation_errors_before_any_output() {
    let mut store = source();
    store.vertices.remove(&1).unwrap();
    for scheduled in [false, true] {
        let control = StorageReadControl::with_limit(256 << 10);
        let mut reader = store.image_reader(&control).unwrap();
        let mut output = [0x6a; 512];
        if scheduled {
            loop {
                match reader.prepare(1) {
                    Ok(progress) if !progress.ready => {}
                    Ok(_) => panic!("corrupt graph was admitted"),
                    Err(_) => break,
                }
            }
        }
        assert!(reader.read(&mut output).is_err());
        assert_eq!(output, [0x6a; 512]);
        assert_eq!(control.memory().used(), 0);
        assert!(reader.prepare(1).is_err());
    }
}
