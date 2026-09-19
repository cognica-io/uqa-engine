//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native IVF publications preserve the common owner's serial training transitions.

use super::*;
use uqa_storage::ivf_index::{IVFIndex, IVFState};

fn train_stale(index: &mut IVFIndex) {
    if index.state() == IVFState::Stale {
        index.train().unwrap();
    }
}

#[test]
fn independent_native_ivf_writers_merge_shared_training_generations() {
    for mode in MODES {
        for seed in [0, 2, 8] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("ivf-merge.db");
            let connection = open(mode, &path);
            Catalog::open(connection.clone()).unwrap();
            bind(&connection);
            let mut left = index(&connection, IndexKind::Ivf, "docs");
            let mut serial = IVFIndex::with_params(3, 2, 2, 2);
            for document in 1..=seed {
                let vector = vec![1.0, document as f32, 0.5];
                left.add(document, vector.clone()).unwrap();
                serial.add(document, vector).unwrap();
                train_stale(&mut serial);
            }
            left.initialize().unwrap();
            serial.initialize().unwrap();
            let other = open(mode, &path);
            bind(&other);
            let mut right = index(&other, IndexKind::Ivf, "docs");
            let baseline = left.snapshot().unwrap();
            connection.begin_transaction().unwrap();
            other.begin_transaction().unwrap();
            left.add_many(11, vec![X.to_vec(), Y.to_vec()]).unwrap();
            let private = left.snapshot().unwrap();
            right.add(12, Z.to_vec()).unwrap();
            other.commit_transaction().unwrap();
            assert!(connection.in_transaction());
            connection.commit_transaction().unwrap();
            serial.add(12, Z.to_vec()).unwrap();
            train_stale(&mut serial);
            serial.add_many(11, vec![X.to_vec(), Y.to_vec()]).unwrap();
            train_stale(&mut serial);
            ivf::matches_metadata(&connection, &serial.metadata_snapshot());
            assert_eq!(left.count().unwrap(), seed as usize + 3);
            assert_eq!(right.count().unwrap(), seed as usize + 3);
            assert_eq!(baseline.count().unwrap(), seed as usize);
            assert_eq!(private.count().unwrap(), seed as usize + 2);
            drop((left, right, private, baseline, other, connection));
            let reopened = open(mode, &path);
            bind(&reopened);
            ivf::matches_metadata(&reopened, &serial.metadata_snapshot());
            assert_eq!(
                index(&reopened, IndexKind::Ivf, "docs")
                    .search_knn(&X, 100)
                    .unwrap()
                    .len(),
                seed as usize + 2
            );
        }
    }
}

#[test]
fn native_ivf_savepoints_and_publication_retry_preserve_intervening_writes() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("retry.db");
        let a = open(mode, &path);
        Catalog::open(a.clone()).unwrap();
        bind(&a);
        let b = open(mode, &path);
        bind(&b);
        let mut left = index(&a, IndexKind::Ivf, "docs");
        let mut right = index(&b, IndexKind::Ivf, "docs");
        let mut serial = IVFIndex::with_params(3, 2, 2, 2);
        for document in 1..=8 {
            let vector = vec![1.0, document as f32, 0.5];
            left.add(document, vector.clone()).unwrap();
            serial.add(document, vector).unwrap();
            train_stale(&mut serial);
        }
        left.initialize().unwrap();
        serial.initialize().unwrap();
        a.begin_transaction().unwrap();
        a.savepoint("discard").unwrap();
        left.add(99, X.to_vec()).unwrap();
        let discarded = left.snapshot().unwrap();
        a.rollback_to_savepoint("discard").unwrap();
        a.release_savepoint("discard").unwrap();
        left.delete(1).unwrap();
        left.add_many(11, vec![X.to_vec(), Y.to_vec()]).unwrap();
        left.add_many(11, vec![]).unwrap();
        left.add_many(11, vec![Y.to_vec(), Z.to_vec()]).unwrap();
        let private = left.snapshot().unwrap();
        right.add(12, Z.to_vec()).unwrap();
        let before = generation(&b, IndexKind::Ivf, "docs", "embedding");
        b.with_physical(|sqlite| {
            sqlite.execute_batch("CREATE TRIGGER fail_ivf_merge BEFORE INSERT ON _ivf_indexes BEGIN SELECT RAISE(ABORT, 'injected IVF merge failure'); END")?;
            Ok(())
        }).unwrap();
        assert!(a.commit_transaction().is_err());
        assert_eq!(generation(&b, IndexKind::Ivf, "docs", "embedding"), before);
        b.with_physical(|sqlite| {
            sqlite.execute_batch("DROP TRIGGER fail_ivf_merge")?;
            Ok(())
        })
        .unwrap();
        right.add(13, X.to_vec()).unwrap();
        a.commit_transaction().unwrap();
        for (document, vectors) in [(12, vec![Z.to_vec()]), (13, vec![X.to_vec()])] {
            serial.add_many(document, vectors).unwrap();
            train_stale(&mut serial);
        }
        serial.delete(1).unwrap();
        train_stale(&mut serial);
        for vectors in [
            vec![X.to_vec(), Y.to_vec()],
            vec![],
            vec![Y.to_vec(), Z.to_vec()],
        ] {
            serial.add_many(11, vectors).unwrap();
            train_stale(&mut serial);
        }
        ivf::matches_metadata(&a, &serial.metadata_snapshot());
        assert_eq!(discarded.count().unwrap(), 9);
        assert_eq!(private.count().unwrap(), 9);
        assert_eq!(left.count().unwrap(), 11);
        let expected = generation(&a, IndexKind::Ivf, "docs", "embedding");
        drop((left, right, private, discarded, a, b));
        let reopened = open(mode, &path);
        bind(&reopened);
        assert_eq!(
            generation(&reopened, IndexKind::Ivf, "docs", "embedding"),
            expected
        );
    }
}
