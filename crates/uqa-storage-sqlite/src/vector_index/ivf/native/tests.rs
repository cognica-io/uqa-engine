//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::SQLiteIVFIndex;
use crate::{Catalog, ManagedConnection};
use uqa_storage::{mvcc::VersionedSessionOptions, StorageBackendError, VectorIndex};

#[test]
fn native_ivf_probes_and_untrained_search_keep_the_original_reader_control() {
    for threshold in [2, 100] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        Catalog::open(connection.clone()).unwrap();
        let mut index = SQLiteIVFIndex::with_params(
            connection.clone(),
            "docs",
            "embedding",
            2,
            2,
            1,
            threshold,
        );
        index
            .add_many(1, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
            .unwrap();
        index.add(2, vec![-1.0, 0.0]).unwrap();
        index.initialize().unwrap();
        let expected = index.search_knn(&[1.0, 0.0], 3).unwrap();
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let control = connection.retention_control().unwrap();
        let retained = index.snapshot().unwrap();
        let used = control.memory().used();
        assert_eq!(retained.search_knn(&[1.0, 0.0], 3).unwrap(), expected);
        assert_eq!(control.memory().used(), used);
        let held = control
            .memory()
            .reserve(control.memory().limit() - used)
            .unwrap();
        assert!(matches!(
            retained.search_knn(&[1.0, 0.0], 3),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), control.memory().limit());
        drop(held);
        control.cancellation().cancel();
        assert!(matches!(
            retained.search_knn(&[1.0, 0.0], 3),
            Err(StorageBackendError::Cancelled(_))
        ));
        control.cancellation().reset();
        assert_eq!(retained.search_knn(&[1.0, 0.0], 3).unwrap(), expected);
        assert_eq!(control.memory().used(), used);
        index.add(1, vec![-1.0, 0.0]).unwrap();
        assert_eq!(retained.search_knn(&[1.0, 0.0], 3).unwrap(), expected);
        drop((retained, index, connection));
        assert_eq!(control.memory().used(), 0);
    }
}
