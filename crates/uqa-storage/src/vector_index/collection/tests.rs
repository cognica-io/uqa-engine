//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::vector_index::MemoryVectorIndex;

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
