//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{HNSWIndex, IVFIndex};

#[test]
fn immutable_vector_facets_keep_raw_tensors_identity_and_original_controls() {
    for mut live in [
        Box::new(HNSWIndex::new(2)) as Box<dyn VectorIndex>,
        Box::new(IVFIndex::with_params(2, 1, 1, 1)),
    ] {
        live.add_many(7, vec![vec![8.0, 0.0], vec![-0.0, 4.0]])
            .unwrap();
        live.add(9, vec![3.0, 0.0]).unwrap();
        live.add(u64::MAX, vec![0.0, -2.0]).unwrap();
        live.delete(9).unwrap();
        let owner = StorageReadControl::with_limit(1 << 20);
        let retained = live.snapshot_with_control(&owner).unwrap();
        let source = retained.vector_read_snapshot(&owner).unwrap().unwrap();
        let fingerprint = source.corpus_fingerprint(&owner).unwrap();
        assert!(fingerprint.is_some());
        live.add(7, vec![-1.0, 0.0]).unwrap();
        live.delete(u64::MAX).unwrap();
        assert_eq!(source.next_document_after(None, &owner).unwrap(), Some(7));
        assert_eq!(
            source.next_document_after(Some(7), &owner).unwrap(),
            Some(u64::MAX)
        );
        assert_eq!(
            source.next_document_after(Some(u64::MAX), &owner).unwrap(),
            None
        );
        assert_eq!(source.document_vector_count(7, &owner).unwrap(), 2);
        assert_eq!(source.document_vector_count(9, &owner).unwrap(), 0);
        assert!(source.read_vector(7, 2, &owner).unwrap().is_none());
        assert_eq!(
            &*source.read_vector(7, 0, &owner).unwrap().unwrap(),
            &[8.0, 0.0]
        );
        assert_eq!(
            source.read_vector(7, 1, &owner).unwrap().unwrap()[0].to_bits(),
            (-0.0_f32).to_bits()
        );
        let nested = retained.snapshot_with_control(&owner).unwrap();
        let nested_source = nested.vector_read_snapshot(&owner).unwrap().unwrap();
        assert_eq!(
            nested_source.corpus_fingerprint(&owner).unwrap(),
            fingerprint
        );
        drop((retained, nested, source, live));
        assert_eq!(
            &*nested_source
                .read_vector(u64::MAX, 0, &owner)
                .unwrap()
                .unwrap(),
            &[0.0, -2.0]
        );
        let invocation = StorageReadControl::with_limit(1024);
        invocation.cancellation().cancel();
        assert!(nested_source.corpus_fingerprint(&invocation).is_err());
        owner.cancellation().cancel();
        assert!(nested_source
            .read_vector(7, 0, &StorageReadControl::with_limit(1024))
            .is_err());
        drop(nested_source);
        assert_eq!(owner.memory().used(), 0);
    }
}

#[test]
fn canonical_capture_shares_existing_coordinates_under_a_header_only_allowance() {
    for mut live in [
        Box::new(HNSWIndex::new(16_384)) as Box<dyn VectorIndex>,
        Box::new(IVFIndex::with_params(16_384, 1, 1, 1)),
    ] {
        live.add(3, vec![2.0; 16_384]).unwrap();
        let owner = StorageReadControl::with_limit(2 << 20);
        let snapshot = live.snapshot_with_control(&owner).unwrap();
        let pressure = owner
            .memory()
            .reserve((2 << 20) - owner.memory().used() - 2048)
            .unwrap();
        let source = snapshot.vector_read_snapshot(&owner).unwrap().unwrap();
        assert_eq!(source.document_vector_count(3, &owner).unwrap(), 1);
        assert!(source.corpus_fingerprint(&owner).unwrap().is_some());
        drop((pressure, snapshot, live));
        assert_eq!(
            source.read_vector(3, 0, &owner).unwrap().unwrap()[16_383],
            2.0
        );
        drop(source);
        assert_eq!(owner.memory().used(), 0);
    }
}
