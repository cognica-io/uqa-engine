//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::MemoryError;

#[test]
fn failed_beams_discard_workspace_preserve_errors_and_cannot_resume() {
    let physical = MemoryBudget::new(1 << 20);
    let (memory, manifest) = fixture(512, 9, &oracle().cases[1], &physical);
    for fault in [1, 2, 3] {
        let owner = StorageReadControl::with_limit(65_536);
        let source = Arc::new(Source::new(memory.clone(), false));
        let reader = reader(source.clone(), &manifest, 0, &owner);
        let control = StorageReadControl::with_limit(65_536);
        let navigation = query(512, &control);
        let mut traversal = DiskANNTraversal::new(reader, &navigation, &control).unwrap();
        drop(navigation);
        assert_eq!(ids(&traversal.next_beam().unwrap()), [6]);
        let before = traversal.stats();
        source.fault.store(fault, AtomicOrdering::Relaxed);
        let error = traversal.next_beam().unwrap_err();
        match fault {
            1 => assert!(
                error.to_string().contains("missing page completion"),
                "{error}"
            ),
            2 => assert!(matches!(
                error,
                StorageBackendError::Memory(MemoryError::Limit {
                    required: 42,
                    limit: 17
                })
            )),
            3 => assert!(matches!(error, StorageBackendError::Cancelled(_))),
            _ => unreachable!(),
        }
        assert_eq!(traversal.stats(), before);
        assert_eq!(control.memory().used(), 0);
        source.fault.store(0, AtomicOrdering::Relaxed);
        let requests = source.requests.lock().unwrap().len();
        assert!(traversal.next_beam().is_err());
        assert!(traversal.complete_next_beam().is_err());
        assert_eq!(source.requests.lock().unwrap().len(), requests);
        drop(traversal);
        assert_eq!(owner.memory().used(), 0);
    }
}

#[test]
fn original_query_limits_cover_lookup_frontier_membership_pages_and_decoded_nodes() {
    let physical = MemoryBudget::new(1 << 20);
    let (memory, manifest) = fixture(1024, 9, &oracle().cases[2], &physical);
    // The 1,024-dimensional codebook's encoded and decoded buffers coexist during open, outside the query workspace under test.
    let owner = StorageReadControl::with_limit(1 << 20);
    let source = Arc::new(Source::new(memory, true));
    let reader = reader(source, &manifest, 0, &owner);
    let navigation_control = StorageReadControl::with_limit(65_536);
    let navigation = query(1024, &navigation_control);
    let mut rejected = false;
    let mut accepted = false;
    for bytes in [0, 1024, 4096, 8192, 16_384, 32_768, 65_536] {
        let control = StorageReadControl::with_limit(bytes);
        let outcome = (|| -> StorageBackendResult<()> {
            let mut traversal = DiskANNTraversal::new(reader.clone(), &navigation, &control)?;
            while !traversal.next_beam()?.is_empty() {}
            while !traversal.complete_next_beam()?.is_empty() {}
            assert_eq!(control.memory().used(), 0);
            Ok(())
        })();
        match outcome {
            Ok(()) => accepted = true,
            Err(StorageBackendError::Memory(_)) => rejected = true,
            Err(error) => panic!("unexpected error: {error}"),
        }
        assert_eq!(control.memory().used(), 0);
        assert!(control.memory().peak() <= bytes);
    }
    assert!(accepted && rejected);
}

#[test]
fn empty_and_singleton_generations_validate_dimensions_and_do_not_replay_nodes() {
    let physical = MemoryBudget::new(65_536);
    for count in [0, 1] {
        let (memory, manifest) = fixture(2, count, &oracle().cases[0], &physical);
        let owner = StorageReadControl::with_limit(65_536);
        let source = Arc::new(Source::new(memory, false));
        let reader = reader(source.clone(), &manifest, 0, &owner);
        let control = StorageReadControl::with_limit(65_536);
        assert!(DiskANNTraversal::new(reader.clone(), &query(3, &control), &control).is_err());
        let mut traversal = DiskANNTraversal::new(reader, &query(2, &control), &control).unwrap();
        assert_eq!(traversal.next_beam().unwrap().len(), count);
        assert!(traversal.next_beam().unwrap().is_empty());
        assert!(traversal.complete_next_beam().unwrap().is_empty());
        assert!(traversal.complete_next_beam().unwrap().is_empty());
        assert_eq!(source.requests.lock().unwrap().len(), count);
        assert_eq!(control.memory().used(), 0);
        control.cancellation().cancel();
        assert!(matches!(
            traversal.next_beam(),
            Err(StorageBackendError::Cancelled(_))
        ));
    }
    assert_eq!(physical.used(), 0);
}

#[test]
fn completion_requires_exhaustion_and_cannot_switch_back_to_approximate_work() {
    let physical = MemoryBudget::new(65_536);
    let (memory, manifest) = fixture(2, 9, &oracle().cases[0], &physical);
    let owner = StorageReadControl::with_limit(65_536);
    let source = Arc::new(Source::new(memory, false));
    let reader = reader(source, &manifest, 0, &owner);
    let control = StorageReadControl::with_limit(65_536);
    let mut early = DiskANNTraversal::new(reader.clone(), &query(2, &control), &control).unwrap();
    assert!(early.complete_next_beam().is_err());
    assert!(early.next_beam().is_err());
    assert_eq!(control.memory().used(), 0);
    let mut traversal = DiskANNTraversal::new(reader, &query(2, &control), &control).unwrap();
    while !traversal.next_beam().unwrap().is_empty() {}
    assert_eq!(ids(&traversal.complete_next_beam().unwrap()), [2]);
    assert!(traversal.next_beam().is_err());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn retained_reader_survives_prior_query_and_opening_cancellation() {
    let physical = MemoryBudget::new(65_536);
    let (memory, manifest) = fixture(2, 9, &oracle().cases[1], &physical);
    let owner = StorageReadControl::with_limit(65_536);
    let source = Arc::new(Source::new(memory, false));
    let reader = reader(source.clone(), &manifest, 8192, &owner);
    let old = StorageReadControl::with_limit(65_536);
    let mut traversal = DiskANNTraversal::new(reader.clone(), &query(2, &old), &old).unwrap();
    assert_eq!(ids(&traversal.next_beam().unwrap()), [6]);
    old.cancellation().cancel();
    assert!(matches!(
        traversal.next_beam(),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(old.memory().used(), 0);
    drop(traversal);
    owner.cancellation().cancel();
    drop(source);
    let current = StorageReadControl::with_limit(65_536);
    let mut traversal =
        DiskANNTraversal::new(reader.clone(), &query(2, &current), &current).unwrap();
    drop(reader);
    let mut order = Vec::new();
    loop {
        let nodes = traversal.next_beam().unwrap();
        if nodes.is_empty() {
            break;
        }
        order.extend(ids(&nodes));
    }
    assert_eq!(order, oracle().cases[1].expanded);
    drop(traversal);
    assert_eq!(current.memory().used(), 0);
    assert_eq!(owner.memory().used(), 0);
    assert_eq!(physical.used(), 0);
}
