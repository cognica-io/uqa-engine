//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn every_training_quota_boundary_releases_unpublished_buffers() {
    let inputs = StorageReadControl::with_limit(4096);
    let rows =
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]].map(|row| navigation(&row, &inputs));
    let mut passed = 0;
    let mut rejected = 0;
    for limit in (0..=4096).step_by(8) {
        let control = StorageReadControl::with_limit(limit);
        let result = (|| {
            let mut trainer = PQTrainer::new(3, 2, options(2), &control)?;
            for row in &rows {
                trainer.observe(row)?;
            }
            trainer.finish()
        })();
        match result {
            Ok(book) => {
                passed += 1;
                drop(book);
            }
            Err(StorageBackendError::Memory(_)) => rejected += 1,
            Err(error) => panic!("unexpected error: {error}"),
        }
        assert_eq!(control.memory().used(), 0, "limit {limit}");
    }
    assert!(passed > 0 && rejected > 0);
}

#[test]
fn failed_reservoir_replacement_preserves_prior_samples_and_random_stream() {
    let inputs = StorageReadControl::with_limit(4096);
    let control = StorageReadControl::with_limit(4096);
    let rows = [
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, -1.0],
    ]
    .map(|row| navigation(&row, &inputs));
    let mut trainer = PQTrainer::new(
        3,
        2,
        PQTrainingOptions {
            max_samples: 5,
            ..options(2)
        },
        &control,
    )
    .unwrap();
    for row in &rows[..5] {
        trainer.observe(row).unwrap();
    }
    let previous: Vec<_> = trainer.samples.iter().map(|row| row.to_vec()).collect();
    let held = control
        .memory()
        .reserve(4096 - control.memory().used())
        .unwrap();
    assert!(matches!(
        trainer.observe(&rows[5]),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(trainer.observed_vectors(), 5);
    assert_eq!(control.memory().used(), 4096);
    for (actual, expected) in trainer.samples.iter().zip(previous) {
        assert_eq!(&**actual, expected);
    }
    drop(held);
    trainer.observe(&rows[5]).unwrap();
    assert_eq!(&*trainer.samples[1], rows[5].coordinates());
    assert_eq!(trainer.observed_vectors(), 6);
    drop(trainer.finish().unwrap());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn query_buffers_retain_their_budget_and_never_capture_training_cancellation() {
    let training = StorageReadControl::with_limit(4096);
    let inputs = StorageReadControl::with_limit(4096);
    let vector = navigation(&[1.0, 0.0, 0.0], &inputs);
    let mut trainer = PQTrainer::new(3, 2, options(2), &training).unwrap();
    trainer.observe(&vector).unwrap();
    let book = trainer.finish().unwrap();
    let retained = training.memory().used();
    assert!(retained >= 3 * size_of::<f64>());
    training.cancellation().cancel();
    for limit in 0..32 {
        let query = StorageReadControl::with_limit(limit);
        let code = book.encode(&vector, &query);
        if let Err(error) = &code {
            assert!(matches!(error, StorageBackendError::Memory(_)));
        }
        drop(code);
        assert_eq!(query.memory().used(), 0);
        let lookup = book.lookup(&vector, &query);
        if let Err(error) = &lookup {
            assert!(matches!(error, StorageBackendError::Memory(_)));
        }
        drop(lookup);
        assert_eq!(query.memory().used(), 0);
        assert_eq!(training.memory().used(), retained);
    }
    let query = StorageReadControl::with_limit(1024);
    let code = book.encode(&vector, &query).unwrap();
    let lookup = book.lookup(&vector, &query).unwrap();
    assert!(query.memory().used() >= 2 + 2 * size_of::<f64>());
    query.cancellation().cancel();
    assert!(matches!(
        book.encode(&vector, &query),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        book.lookup(&vector, &query),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        lookup.estimate(&code, &query),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop((lookup, code, book));
    assert_eq!(query.memory().used(), 0);
    assert_eq!(training.memory().used(), 0);
}

#[test]
fn cancellation_preserves_admitted_samples_and_releases_failed_training() {
    let inputs = StorageReadControl::with_limit(4096);
    let control = StorageReadControl::with_limit(4096);
    let vector = navigation(&[1.0, 0.0], &inputs);
    let mut trainer = PQTrainer::new(2, 1, options(2), &control).unwrap();
    trainer.observe(&vector).unwrap();
    let retained = control.memory().used();
    control.cancellation().cancel();
    assert!(matches!(
        trainer.observe(&vector),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(trainer.observed_vectors(), 1);
    assert_eq!(control.memory().used(), retained);
    assert!(matches!(
        trainer.finish(),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}
