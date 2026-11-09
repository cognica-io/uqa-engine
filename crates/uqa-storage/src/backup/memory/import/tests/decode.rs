//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn native_codec_input_borrows_the_admitted_address_and_requires_verified_eof() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let repository = repository(&directory.path().join("repository"), &control);
    let root = publish(&repository, cut(), catalogs(), &control);
    for consume in [false, true] {
        let input = StorageReadControl::with_limit(1 << 20);
        let lease = repository.open_backup(root.backup(), &input).unwrap();
        let mut reader = MemoryImageImportReader::new(lease, 65536, &input).unwrap();
        loop {
            if reader.advance(64).unwrap() == BackupSourceStep::Unit {
                break;
            }
        }
        let result = reader.decode_current(|unit, payload| {
            assert!(matches!(
                unit.address,
                DecodedMemoryImageAddress::Catalog(1)
            ));
            assert_eq!(unit.decoded_bytes, 1);
            if consume {
                let mut byte = [0];
                payload.read_exact(&mut byte)?;
                assert_eq!(byte, [1]);
            }
            Ok(())
        });
        if consume {
            result.unwrap();
            assert!(reader.current_unit().is_some());
            assert!(reader.header().is_some());
        } else {
            assert!(result.is_err());
            assert!(reader.current_unit().is_none());
            assert!(reader.header().is_none());
            assert!(matches!(reader.advance(64), Err(BackupError::Finished)));
        }
        drop(reader);
        assert_eq!(input.memory().used(), 0);
    }
}

#[test]
fn native_codec_failure_cancellation_and_unwind_retire_the_repository_input() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let repository = repository(&directory.path().join("repository"), &control);
    let root = publish(&repository, cut(), catalogs(), &control);
    for failure in 0..3 {
        let input = StorageReadControl::with_limit(1 << 20);
        let lease = repository.open_backup(root.backup(), &input).unwrap();
        let mut reader = MemoryImageImportReader::new(lease, 65536, &input).unwrap();
        loop {
            if reader.advance(64).unwrap() == BackupSourceStep::Unit {
                break;
            }
        }
        match failure {
            0 => assert!(matches!(
                reader.decode_current::<()>(|_, _| Err(BackupError::Invalid("native codec"))),
                Err(BackupError::Invalid("native codec"))
            )),
            1 => {
                input.cancellation().cancel();
                let result = reader.decode_current(|_, payload| {
                    // A codec cannot turn a failed verified read into successful input.
                    let _ = payload.read(&mut [0]);
                    Ok(())
                });
                assert!(matches!(result, Err(BackupError::Cancelled(_))));
            }
            _ => assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = reader.decode_current::<()>(|_, _| panic!("native codec unwind"));
            }))
            .is_err()),
        }
        assert!(reader.current_unit().is_none());
        assert!(reader.header().is_none());
        assert!(matches!(reader.advance(64), Err(BackupError::Finished)));
        assert_eq!(input.memory().used(), 0);
    }
}
