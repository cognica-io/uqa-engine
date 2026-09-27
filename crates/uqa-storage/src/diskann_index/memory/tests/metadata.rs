//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn diskann_calibration_metadata_distinguishes_mutations_generations_and_controls() {
    let owner = StorageReadControl::with_limit(1 << 20);
    let mut index = new(&owner);
    index.add(1, vec![1.0, 0.0]).unwrap();
    let retained = index.snapshot().unwrap();
    let before = retained.diskann_query_metadata(&owner).unwrap().unwrap();
    assert!(before.corpus_fingerprint.is_some());
    index.add(2, vec![0.0, 1.0]).unwrap();
    let changed = index.diskann_query_metadata(&owner).unwrap().unwrap();
    assert_ne!(before.corpus_fingerprint, changed.corpus_fingerprint);
    assert_eq!(
        before.index_fingerprint(&owner).unwrap(),
        changed.index_fingerprint(&owner).unwrap()
    );
    index.initialize().unwrap();
    let rebuilt = index.diskann_query_metadata(&owner).unwrap().unwrap();
    assert_eq!(changed.corpus_fingerprint, rebuilt.corpus_fingerprint);
    assert_ne!(
        changed.index_fingerprint(&owner).unwrap(),
        rebuilt.index_fingerprint(&owner).unwrap()
    );
    assert_eq!(
        retained.diskann_query_metadata(&owner).unwrap().unwrap(),
        before
    );
    let invocation = StorageReadControl::with_limit(1 << 20);
    invocation.cancellation().cancel();
    assert!(retained.diskann_query_metadata(&invocation).is_err());
    owner.cancellation().cancel();
    assert!(retained
        .diskann_query_metadata(&StorageReadControl::with_limit(1 << 20))
        .is_err());
}
