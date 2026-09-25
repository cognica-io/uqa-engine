//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::MemoryError;

#[test]
fn temporary_limits_are_shared_and_fail_before_growing_either_capture() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(4096);
    let limit = temporary::File::physical_len_for(52).unwrap() * 2;
    let temporary = DiskANNTemporaryBudget::new(limit);
    let input = fixture(directory.path(), &temporary, &control).unwrap();
    assert_eq!(temporary.used(), limit);
    let cloned = temporary.clone();
    let error = fixture(directory.path(), &cloned, &control).err().unwrap();
    let StorageBackendError::Backend { source, .. } = error else {
        panic!("expected typed temporary limit");
    };
    assert_eq!(
        source.downcast_ref::<DiskANNTemporaryError>(),
        Some(&DiskANNTemporaryError::Limit {
            required: limit + limit / 2,
            limit,
        })
    );
    assert_eq!(temporary.limit(), limit);
    assert_eq!(temporary.peak(), limit);
    assert_eq!(cloned.used(), limit);
    assert_eq!(input.read_node(0).unwrap().doc_id(), 10);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    drop(input);
    assert_eq!(cloned.used(), 0);
    assert!(empty(directory.path()));
}

#[test]
fn suppressed_consumer_failures_keep_the_first_typed_error_and_remove_all_files() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(0);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let result = DiskANNBuildInput::capture(
        generation(),
        2,
        directory.path(),
        &temporary,
        &control,
        |visitor| {
            assert!(visitor(1, 0, version(), &[1.0, 0.0]).is_err());
            control.cancellation().cancel();
            assert!(visitor(2, 0, version(), &[1.0, 0.0]).is_err());
            Ok(())
        },
    );
    assert!(matches!(
        result,
        Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(temporary.used(), 0);
    assert!(empty(directory.path()));
}

#[test]
fn rejected_order_values_source_failure_and_cancellation_discard_the_capture() {
    for failure in 0..7 {
        let directory = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(4096);
        let temporary = DiskANNTemporaryBudget::new(1 << 20);
        let result = DiskANNBuildInput::capture(
            generation(),
            2,
            directory.path(),
            &temporary,
            &control,
            |visitor| {
                visitor(10, 0, version(), &[1.0, 0.0])?;
                match failure {
                    0 => visitor(9, 0, version(), &[1.0, 0.0]),
                    1 => visitor(10, 2, version(), &[1.0, 0.0]),
                    2 => visitor(11, 1, version(), &[1.0, 0.0]),
                    3 => visitor(11, 0, version(), &[1.0]),
                    4 => visitor(11, 0, version(), &[f32::NAN, 0.0]),
                    5 => Err(temporary::io_error(std::io::Error::other(
                        "source read failed",
                    ))),
                    _ => {
                        control.cancellation().cancel();
                        visitor(11, 0, version(), &[1.0, 0.0])
                    }
                }
            },
        );
        assert!(result.is_err(), "failure={failure}");
        assert_eq!(control.memory().used(), 0);
        assert_eq!(temporary.used(), 0);
        assert!(empty(directory.path()));
    }
}

#[test]
fn captured_reads_keep_original_limits_and_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(4096);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let input = fixture(directory.path(), &temporary, &control).unwrap();
    let held = control.memory().reserve(4096).unwrap();
    assert!(matches!(
        input.read_node(0),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(matches!(
        input.train(1, options()),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), 4096);
    drop(held);
    assert_eq!(input.read_node(0).unwrap().doc_id(), 10);
    control.cancellation().cancel();
    assert!(matches!(
        input.read_side(0),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        input.train(1, options()),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop(input);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(temporary.used(), 0);
    assert!(empty(directory.path()));
}

#[test]
fn corrupt_or_truncated_ciphertext_cannot_become_an_empty_record_stream() {
    for truncate in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(4096);
        let temporary = DiskANNTemporaryBudget::new(1 << 20);
        let input = fixture(directory.path(), &temporary, &control).unwrap();
        let path = input.navigation.path();
        let mut bytes = std::fs::read(path).unwrap();
        if truncate {
            bytes.truncate(bytes.len() - 1);
        } else {
            bytes[1 + 24 + 2] ^= 1;
            bytes[1 + (24 + 2 + 16 + 4096) + 24 + 2] ^= 1;
        }
        std::fs::write(path, bytes).unwrap();
        assert!(matches!(
            input.read_node(0),
            Err(StorageBackendError::Backend { .. })
        ));
        assert!(matches!(
            input.train(1, options()),
            Err(StorageBackendError::Backend { .. })
        ));
        assert_eq!(control.memory().used(), 0);
        drop(input);
        assert_eq!(temporary.used(), 0);
        assert!(empty(directory.path()));
    }
}
