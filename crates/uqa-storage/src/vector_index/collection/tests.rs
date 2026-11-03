//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::vector_index::MemoryVectorIndex;

fn encoded(index: &dyn VectorIndex) -> Vec<u8> {
    let mut bytes = Vec::new();
    index
        .write_image(&mut bytes, &StorageReadControl::with_limit(1 << 20))
        .unwrap();
    bytes
}

fn encoded_capture(
    index: &super::capture::CapturedVectorIndexes,
    field: &str,
    current: Option<&VectorIndexes>,
    cut: &crate::memory_capture::MemoryCapture,
) -> Vec<u8> {
    let image = index.image(field, current, cut).unwrap();
    let mut bytes = Vec::new();
    image
        .write_image(&mut bytes, &StorageReadControl::with_limit(1 << 20))
        .unwrap();
    bytes
}

fn capture_sources(
    source_control: &StorageReadControl,
) -> Vec<(&'static str, Box<dyn VectorIndex>)> {
    use crate::vector_index::{DiskANNIndexParams, HNSWIndexParams};
    use crate::{
        diskann_index::{build::DiskANNTemporaryBudget, DiskANNIndexOptions, DiskANNMemoryIndex},
        HNSWIndex, IVFIndex,
    };
    let temporary = DiskANNTemporaryBudget::new(16 << 20);
    vec![
        ("exact", Box::new(MemoryVectorIndex::new(2))),
        ("ivf", Box::new(IVFIndex::with_params(2, 2, 1, 4))),
        (
            "hnsw",
            Box::new(HNSWIndex::with_params(2, HNSWIndexParams::default()).unwrap()),
        ),
        (
            "diskann",
            Box::new(
                DiskANNMemoryIndex::new(
                    2,
                    DiskANNIndexOptions::for_parameters(
                        DiskANNIndexParams::for_dimensions(2).unwrap(),
                    ),
                    &temporary,
                    source_control,
                )
                .unwrap(),
            ),
        ),
    ]
}

#[test]
fn memory_vector_membership_preserves_all_owners_across_replacement_and_source_retirement() {
    use crate::memory_capture::MemoryCaptureRegistry;
    let registry = MemoryCaptureRegistry::new();
    let source_control = StorageReadControl::with_limit(16 << 20);
    let mut indexes = capture_sources(&source_control);
    let source = registry.cell(VectorIndexes::memory(&registry)).unwrap();
    let mut expected = BTreeMap::new();
    for (name, mut index) in indexes.drain(..) {
        for id in 0..10 {
            index.add(id, vec![id as f32, 10.0 - id as f32]).unwrap();
        }
        index.initialize().unwrap();
        expected.insert(name, encoded(index.as_ref()));
        source
            .write()
            .live_mut()
            .unwrap()
            .insert(name.into(), index)
            .unwrap();
    }
    let control = StorageReadControl::with_limit(16 << 20);
    let cut = registry.capture(&control).unwrap();
    assert!(control.memory().used() < 4096);
    let key = source.key().unwrap();
    assert!(key.retained(&cut).unwrap().is_none());
    let detached = source.write().live_mut().unwrap().remove("ivf").unwrap();
    // A removed index still has a live owner; unpublication must already have preserved its image.
    source
        .write()
        .live_mut()
        .unwrap()
        .insert("ivf".into(), Box::new(MemoryVectorIndex::new(3)))
        .unwrap();
    let captured = source.captured(&cut).unwrap();
    assert_eq!(
        captured.fields().unwrap().collect::<Vec<_>>(),
        ["diskann", "exact", "hnsw", "ivf"]
    );
    for (name, bytes) in &expected {
        assert_eq!(
            &encoded_capture(&captured, name, Some(&source.read()), &cut),
            bytes,
            "{name}"
        );
    }
    let next = registry.capture(&control).unwrap();
    assert!(captured
        .image("exact", Some(&source.read()), &next)
        .is_err());
    assert!(captured.image("ivf", Some(&source.read()), &next).is_err());
    let current = source
        .read()
        .iter()
        .map(|(name, index)| (name.clone(), encoded(index)))
        .collect::<BTreeMap<_, _>>();
    source
        .write()
        .live_mut()
        .unwrap()
        .replace(BTreeMap::new())
        .unwrap();
    drop((detached, source, registry));
    let next_captured = key.retained(&next).unwrap().unwrap();
    for (name, bytes) in current {
        assert_eq!(
            encoded_capture(&next_captured, &name, None, &next),
            bytes,
            "{name}"
        );
    }
    for (name, bytes) in expected {
        assert_eq!(
            encoded_capture(&captured, name, None, &cut),
            bytes,
            "{name}"
        );
    }
    drop((captured, next_captured, next, cut));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(source_control.memory().used(), 0);
}

#[test]
fn memory_vector_membership_retains_ivf_query_transitions_before_collection_preservation() {
    use crate::{memory_capture::MemoryCaptureRegistry, IVFIndex};
    let registry = MemoryCaptureRegistry::new();
    let mut ivf = IVFIndex::with_params(2, 2, 1, 4);
    for id in 0..10 {
        ivf.add(id, vec![id as f32, 10.0 - id as f32]).unwrap();
    }
    ivf.train().unwrap();
    for id in 0..3 {
        ivf.delete(id).unwrap();
    }
    let original = encoded(&ivf);
    let expected = ivf
        .writable_snapshot()
        .unwrap()
        .search_knn(&[1.0, 0.0], 3)
        .unwrap();
    let source = registry.cell(VectorIndexes::memory(&registry)).unwrap();
    source
        .write()
        .live_mut()
        .unwrap()
        .insert("v".into(), Box::new(ivf))
        .unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let cut = registry.capture(&control).unwrap();
    assert_eq!(
        source.read()["v"].search_knn(&[1.0, 0.0], 3).unwrap(),
        expected
    );
    assert!(source.key().unwrap().retained(&cut).unwrap().is_none());
    assert_ne!(encoded(&source.read()["v"]), original);
    let captured = source.captured(&cut).unwrap();
    assert_eq!(
        encoded_capture(&captured, "v", Some(&source.read()), &cut),
        original
    );
    source
        .write()
        .live_mut()
        .unwrap()
        .get_mut("v")
        .unwrap()
        .clear()
        .unwrap();
    assert_eq!(
        encoded_capture(&captured, "v", Some(&source.read()), &cut),
        original
    );
}

#[test]
fn memory_vector_membership_rejects_foreign_registration_without_changing_current_state() {
    use crate::memory_capture::MemoryCaptureRegistry;
    let registry = MemoryCaptureRegistry::new();
    let other = MemoryCaptureRegistry::new();
    let source = registry.cell(VectorIndexes::memory(&registry)).unwrap();
    source
        .write()
        .live_mut()
        .unwrap()
        .insert("v".into(), Box::new(MemoryVectorIndex::new(2)))
        .unwrap();
    let foreign = Box::new(MemoryVectorIndex::new(3))
        .into_memory_capture(&other)
        .unwrap();
    assert!(source
        .write()
        .live_mut()
        .unwrap()
        .insert("v".into(), foreign)
        .is_err());
    assert_eq!(source.read()["v"].dimensions(), 2);
    for id in 0..100 {
        source
            .write()
            .live_mut()
            .unwrap()
            .get_mut("v")
            .unwrap()
            .add(id, vec![1.0, 0.0])
            .unwrap();
    }
    let control = StorageReadControl::with_limit(1024);
    let cut = registry.capture(&control).unwrap();
    source
        .write()
        .live_mut()
        .unwrap()
        .get_mut("v")
        .unwrap()
        .add(100, vec![1.0, 0.0])
        .unwrap();
    assert_eq!(source.read()["v"].count().unwrap(), 101);
    // An underfunded archive must not prevent the mutation, retirement or replacement of the source.
    assert!(cut.check().is_err());
    source.write().live_mut().unwrap().remove("v");
    assert!(source.read().is_empty());
    drop(cut);
    assert_eq!(control.memory().used(), 0);
}

fn source() -> VectorIndexes {
    BTreeMap::from([
        (
            "z".into(),
            Box::new(MemoryVectorIndex::new(3)) as Box<dyn VectorIndex>,
        ),
        (
            "a".into(),
            Box::new(MemoryVectorIndex::new(2)) as Box<dyn VectorIndex>,
        ),
    ])
    .into()
}

#[test]
fn empty_retained_collections_need_no_heap_allowance_and_stay_immutable() {
    let control = StorageReadControl::with_limit(0);
    let mut retained = RetainedVectorIndexesBuilder::new(&control)
        .finish()
        .unwrap();
    assert!(retained.live_mut().is_err());
    let nested = VectorIndexes::capture(&retained, &control).unwrap();
    assert!(nested.is_empty());
    assert_eq!(nested.iter().len(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn nested_collections_share_nodes_names_and_handles_at_full_allowance() {
    let source = source();
    let control = StorageReadControl::with_limit(64 * 1024);
    let retained = VectorIndexes::capture(&source, &control).unwrap();
    let bytes = control.memory().used();
    assert!(bytes > 2 * size_of::<(String, Box<dyn VectorIndex>)>());
    assert_eq!(
        retained.keys().map(String::as_str).collect::<Vec<_>>(),
        ["a", "z"]
    );
    let remaining = control
        .memory()
        .reserve(control.memory().limit() - bytes)
        .unwrap();
    let nested = VectorIndexes::capture(&retained, &control).unwrap();
    assert_eq!(control.memory().used(), control.memory().limit());
    assert!(std::ptr::eq(
        retained.get("a").unwrap(),
        nested.get("a").unwrap()
    ));
    drop(remaining);
    drop((source, retained));
    assert_eq!(control.memory().used(), bytes);
    assert_eq!(nested["a"].dimensions(), 2);
    assert_eq!(nested["z"].dimensions(), 3);
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn map_node_admission_is_required_after_name_and_handle_admission() {
    let field = String::from("v");
    let metadata = field.capacity()
        + size_of::<ReadOnlySnapshot<MemoryVectorIndex>>()
        + size_of::<MemoryVectorIndex>()
        + size_of::<MemoryReservation>();
    let control = StorageReadControl::with_limit(metadata);
    let name = control.memory().reserve(field.capacity()).unwrap();
    let mut builder = RetainedVectorIndexesBuilder::new(&control);
    let error = builder
        .insert_admitted(field, MemoryVectorIndex::new(2), name)
        .unwrap_err();
    assert!(matches!(error, StorageBackendError::Memory(_)));
    assert_eq!(control.memory().peak(), metadata);
    assert_eq!(control.memory().used(), 0);
    assert!(builder.indexes.is_empty());
}

#[test]
fn individual_index_snapshots_do_not_keep_dead_collection_nodes() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let retained = VectorIndexes::capture(&source(), &control).unwrap();
    let bytes = control.memory().used();
    let index = retained["a"].snapshot().unwrap();
    drop(retained);
    assert!(control.memory().used() > 0);
    assert!(control.memory().used() < bytes);
    assert_eq!(index.dimensions(), 2);
    drop(index);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn partial_capture_unwinds_nodes_and_names_when_a_later_field_exceeds_quota() {
    let source: LiveIndexes = BTreeMap::from([
        (
            "a".into(),
            Box::new(MemoryVectorIndex::new(2)) as Box<dyn VectorIndex>,
        ),
        (
            "z".repeat(64 * 1024),
            Box::new(MemoryVectorIndex::new(2)) as Box<dyn VectorIndex>,
        ),
    ]);
    let control = StorageReadControl::with_limit(4096);
    assert!(matches!(
        VectorIndexes::capture(&source, &control),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(control.memory().peak() > 0);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(source.len(), 2);
}

#[test]
fn retained_collections_reject_mutation_and_preserve_original_cancellation() {
    let mut live = source();
    live.live_mut().unwrap().remove("z");
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut retained = VectorIndexes::capture(&live, &control).unwrap();
    assert!(retained.live_mut().is_err());
    assert_eq!(retained.len(), 1);
    let nested_control = StorageReadControl::with_limit(0);
    let nested = VectorIndexes::capture(&retained, &nested_control).unwrap();
    assert_eq!(nested_control.memory().used(), 0);
    control.cancellation().cancel();
    assert!(matches!(
        VectorIndexes::capture(&nested, &nested_control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        retained.live_mut(),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop((nested, retained));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn foreign_or_incomplete_name_leases_do_not_reach_the_collection() {
    let control = StorageReadControl::with_limit(4096);
    let foreign = StorageReadControl::with_limit(4096);
    let mut builder = RetainedVectorIndexesBuilder::new(&control);
    for memory in [
        foreign.memory().reserve(100).unwrap(),
        control.memory().empty_reservation(),
    ] {
        assert!(builder
            .insert_admitted("field".into(), MemoryVectorIndex::new(2), memory)
            .is_err());
        assert!(builder.indexes.is_empty());
    }
    assert_eq!(control.memory().used(), 0);
    assert_eq!(foreign.memory().used(), 0);
}

#[test]
fn custom_sources_cannot_bypass_admission_by_labeling_live_registrations_retained() {
    struct InvalidSource;
    impl VectorIndexSource for InvalidSource {
        fn visit(
            &self,
            _: &mut dyn FnMut(&str, &dyn VectorIndex) -> StorageBackendResult<()>,
        ) -> StorageBackendResult<()> {
            panic!("invalid retained result must fail before visiting live providers")
        }
        fn retained_collection(&self) -> StorageBackendResult<Option<VectorIndexes>> {
            Ok(Some(source()))
        }
    }
    let control = StorageReadControl::with_limit(0);
    assert!(VectorIndexes::capture(&InvalidSource, &control).is_err());
    assert_eq!(control.memory().used(), 0);
}
