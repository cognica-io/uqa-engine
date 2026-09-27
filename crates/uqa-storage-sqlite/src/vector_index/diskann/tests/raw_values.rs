//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::VectorIndex;

#[test]
fn native_diskann_raw_values_follow_the_exact_snapshot_in_all_file_modes() {
    let directory = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        let connection = open(
            &directory.path().join(format!("raw-values-{mode}.db")),
            mode,
        );
        let mut index = SQLiteVectorIndex::new(connection.clone(), "docs", "embedding", 2);
        index
            .add_many(1, vec![vec![-0.0, f32::MAX], vec![1.0, f32::from_bits(1)]])
            .unwrap();
        index.add(2, vec![0.0, 1.0]).unwrap();
        let old = index.snapshot().unwrap();
        index.clear().unwrap();
        index.add(3, vec![-1.0, 0.0]).unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let source = old.vector_read_snapshot(&control).unwrap().unwrap();
        drop((old, index, connection));
        assert_eq!(source.next_document_after(None, &control).unwrap(), Some(1));
        assert_eq!(
            source.next_document_after(Some(1), &control).unwrap(),
            Some(2)
        );
        assert_eq!(source.next_document_after(Some(2), &control).unwrap(), None);
        assert_eq!(source.document_vector_count(1, &control).unwrap(), 2);
        assert_eq!(source.document_vector_count(3, &control).unwrap(), 0);
        assert_eq!(
            source
                .read_vector(1, 0, &control)
                .unwrap()
                .unwrap()
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            [(-0.0_f32).to_bits(), f32::MAX.to_bits()]
        );
        assert_eq!(
            source.read_vector(1, 1, &control).unwrap().unwrap()[1].to_bits(),
            1
        );
        assert!(source.read_vector(1, 2, &control).unwrap().is_none());
        let held = control.memory().used();
        let full = control
            .memory()
            .reserve(control.memory().limit() - held)
            .unwrap();
        assert!(source.read_vector(1, 0, &control).is_err());
        drop(full);
        let fresh = StorageReadControl::with_limit(1 << 20);
        control.cancellation().cancel();
        assert!(source.document_vector_count(1, &fresh).is_err());
        assert!(source.read_vector(1, 0, &fresh).is_err());
        drop(source);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(fresh.memory().used(), 0);
    }
}
