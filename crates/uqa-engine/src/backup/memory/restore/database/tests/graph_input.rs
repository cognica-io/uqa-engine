//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeMap;

fn graph_source() -> Engine {
    let source = Engine::new();
    let label = "node한글\0".repeat(257);
    for id in 1..=4 {
        let mut vertex = Vertex::new(id, &label);
        vertex.properties.insert(
            "saved".into(),
            Value::Array(uqa_core::ArrayValue::try_new((0..64).map(Value::Int).collect()).unwrap()),
        );
        source
            .add_graph_vertex(vertex, "saved_graph\0한글")
            .unwrap();
    }
    for id in 1..=3 {
        source
            .add_graph_edge(Edge::new(id, id, id + 1, "edge한글\0"), "saved_graph\0한글")
            .unwrap();
    }
    source
        .build_path_index(
            "saved_path\0한글",
            "saved_graph\0한글",
            &[vec!["edge한글\0".into()], vec!["edge한글\0".into(); 2]],
        )
        .unwrap();
    source
}

#[test]
fn repository_graph_and_path_input_obey_current_windows_after_source_retirement() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = graph_source();
    let expected = image(&source);
    let root = publish(&source, &repository);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&repository, root, &control);
    let mut units = BTreeMap::new();
    let mut calls = 0;
    let state = loop {
        let maximum = if calls % 2 == 0 { 1 } else { 17 };
        calls += 1;
        if let Some(state) = input.advance(maximum).unwrap() {
            break state;
        }
        if let Some(unit) = input.image.as_ref().unwrap().current_unit() {
            if let DecodedMemoryImageAddress::Resource {
                kind: kind @ (1 | 3),
                name,
            } = unit.address
            {
                let entry = units
                    .entry((*kind, name.to_string()))
                    .or_insert((0_u64, 0_u64, 0_u64));
                entry.0 += 1;
                entry.1 += maximum as u64;
                entry.2 = unit.decoded_bytes;
            }
        }
    };
    assert!(units.keys().any(|(kind, _)| *kind == 1));
    assert!(units.keys().any(|(kind, _)| *kind == 3));
    for ((kind, name), (calls, allowed, bytes)) in units {
        assert!(calls > 1, "resource {kind}/{name:?} completed in one call");
        assert!(bytes <= allowed, "resource {kind}/{name:?}: {bytes} bytes consumed with {allowed} bytes of call allowance");
    }
    drop(input);
    let restored = state
        .into_private_engine(MemoryBudget::new(32 << 20), &control)
        .unwrap();
    repository
        .remove_backup(root.backup(), &StorageReadControl::with_limit(1 << 20))
        .unwrap();
    drop(repository);
    control.cancellation().cancel();
    assert_eq!(image(&restored), expected);
    restored
        .add_graph_vertex(Vertex::new(5, "later"), "saved_graph\0한글")
        .unwrap();
    assert_ne!(image(&restored), expected);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn repository_graph_and_path_interruption_retires_the_private_destination() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = graph_source();
    let root = publish(&source, &repository);
    drop(source);
    for kind in [1, 3] {
        for mode in 0..4 {
            let control = StorageReadControl::with_limit(32 << 20);
            let mut input = input(&repository, root, &control);
            let mut calls = 0;
            loop {
                assert!(input.advance(1).unwrap().is_none());
                if matches!(input.image.as_ref().unwrap().current_unit().map(|unit| unit.address), Some(DecodedMemoryImageAddress::Resource { kind: selected, .. }) if *selected == kind)
                {
                    calls += 1;
                    if calls == 30 {
                        break;
                    }
                }
            }
            assert!(control.memory().used() > 0);
            let held = if mode == 1 {
                Some(
                    control
                        .memory()
                        .reserve(control.memory().available())
                        .unwrap(),
                )
            } else {
                None
            };
            if mode == 0 {
                control.cancellation().cancel();
            }
            if mode != 2 {
                while let Ok(state) = input.advance(usize::from(mode != 3)) {
                    assert!(
                        state.is_none(),
                        "interrupted resource cannot publish a destination"
                    );
                }
                assert!(input.candidate.is_none() && input.image.is_none());
                assert!(matches!(input.advance(1), Err(BackupError::Finished)));
            }
            drop(input);
            drop(held);
            assert_eq!(
                control.memory().used(),
                0,
                "resource {kind}, interruption {mode}"
            );
        }
    }
}
