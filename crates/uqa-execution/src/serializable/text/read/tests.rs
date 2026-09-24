//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn controlled_text_capture_keeps_the_owner_allowance_through_observer_snapshots() {
    let mut index = uqa_storage::MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    index
        .add_document(
            7,
            BTreeMap::from([("body".into(), "retained retained".into())]),
        )
        .unwrap();
    let index: Arc<dyn InvertedIndex> = Arc::new(index);
    let observed = ObservedTextIndex::new(index, None, Arc::new(Vec::new()));
    assert!(matches!(
        observed.snapshot_with_control(&StorageReadControl::with_limit(0)),
        Err(StorageBackendError::Memory(_))
    ));
    let control = StorageReadControl::with_limit(1 << 20);
    let retained = observed.snapshot_with_control(&control).unwrap();
    let bytes = control.memory().used();
    assert!(bytes > 0);
    let foreign = StorageReadControl::with_limit(0);
    let nested = retained.snapshot_with_control(&foreign).unwrap();
    assert_eq!(foreign.memory().used(), 0);
    assert_eq!(control.memory().used(), bytes);
    drop(observed);
    drop(retained);
    assert_eq!(nested.get_term_freq(7, "body", "retained").unwrap(), 2);
    control.cancellation().cancel();
    assert!(matches!(
        nested.get_term_freq(7, "body", "retained"),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}
