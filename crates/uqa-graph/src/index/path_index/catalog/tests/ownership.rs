//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{GraphCatalog, GraphStoreHandle};
use uqa_core::{
    image_io::{decode_image, ImageDecodeWindow, ImageDecoder},
    memory::Budgeted,
};
use uqa_storage::catalog::path_index;

fn name(key: &str, control: &StorageReadControl) -> Budgeted<String> {
    ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    )
    .copy_text(key)
    .unwrap()
    .into_budgeted()
    .unwrap()
}

fn graphs(names: &[&str], control: &StorageReadControl) -> GraphCatalog {
    let mut graphs = GraphCatalog::new(control.memory());
    let store = Arc::new(GraphStoreHandle::default());
    for graph in names {
        graphs.insert(graph, store.clone()).unwrap();
    }
    graphs
}

#[test]
fn graph_path_removal_preserves_literal_encoded_prefixes_and_distinct_owners() {
    let graph = "a::b";
    let literal = path_index::prefixes(graph).1;
    let keys = [
        path_index::key(graph, "c"),
        path_index::key(&literal, "c"),
        path_index::key("a", "b::c"),
        path_index::key("a", "keep"),
    ];
    assert!(keys[1].starts_with(&literal) && keys[1].contains("::"));
    let mut catalog = PathIndexCatalog::default();
    for key in &keys {
        catalog.insert(key, materialized("first")).unwrap();
    }
    let original = catalog.clone();
    catalog.remove_graph(graph).unwrap();
    assert!(!catalog.contains_key(&keys[0]));
    assert!(keys[1..].iter().all(|key| catalog.contains_key(key)));
    catalog.remove_graph("a").unwrap();
    assert_eq!(catalog.keys().collect::<Vec<_>>(), [&keys[1]]);
    assert_eq!(original.len(), 4);
}

#[test]
fn restored_path_images_resolve_legacy_names_against_the_original_graphs() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut payload = Vec::new();
    materialized("edge")
        .write_image(&mut payload, &control)
        .unwrap();
    let read = |owners: &[&str], key: &str| {
        let graphs = graphs(owners, &control);
        let paths = PathIndexCatalog::new(control.memory());
        let key = name(key, &control);
        decode_image(
            &mut &payload[..],
            control.memory(),
            control.cancellation(),
            async |mut input| paths.read_graph_image_from(key, &graphs, &mut input).await,
        )
    };
    let key = "a::b::c";
    for (graph, index) in [("a", "b::c"), ("a::b", "c")] {
        let physical = path_index::key(graph, index);
        assert_ne!(physical, key);
        let catalog = read(&[graph], key).unwrap();
        assert!(catalog.get(&physical).unwrap().has_path(&["edge".into()]));
        assert!(!catalog.contains_key(key));
    }
    for (owners, key, error) in [
        (&["a", "a::b"][..], key, "ambiguous"),
        (&[][..], "a::c", "missing graph"),
        (&["a"][..], "opaque", "invalid path-index key"),
    ] {
        assert!(read(owners, key).err().unwrap().to_string().contains(error));
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn path_owner_resolution_yields_and_retires_after_cancellation_or_denial() {
    let graph = "graph".repeat(8192);
    let raw = format!("{graph}::one::two");
    for cancel in [true, false] {
        let control = StorageReadControl::with_limit(1 << 20);
        let owners = graphs(&[&graph], &control);
        let key = name(&raw, &control);
        let paths = PathIndexCatalog::new(control.memory());
        let mut decoder = ImageDecoder::<_, StorageBackendError>::new(
            control.memory(),
            control.cancellation(),
            move |mut channel| async move {
                paths
                    .read_graph_image_from(key, &owners, &mut channel.input())
                    .await
            },
        )
        .unwrap();
        let exhausted = control
            .memory()
            .reserve(control.memory().available())
            .unwrap();
        let mut steps = 0;
        loop {
            match decoder.advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 },
            ) {
                Ok(None) => {
                    steps += 1;
                    if cancel && steps == 16 {
                        control.cancellation().cancel();
                    }
                }
                Err(StorageBackendError::Cancelled(_)) => {
                    assert!(cancel);
                    break;
                }
                Err(StorageBackendError::Memory(_)) => {
                    assert!(!cancel && steps > 100);
                    break;
                }
                _ => panic!("the unadmitted legacy registration must not be published"),
            }
        }
        drop((decoder, exhausted));
        assert_eq!(control.memory().used(), 0);
    }
}
