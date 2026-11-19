//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn native_codec_steps_retain_the_component_until_explicit_completion() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let repository = repository(&directory.path().join("repository"), &control);
    let root = publish(&repository, cut(), catalogs(), &control);
    for finish in ["complete", "zero", "raw", "error", "unwind", "cancel"] {
        let input = StorageReadControl::with_limit(1 << 20);
        let lease = repository.open_backup(root.backup(), &input).unwrap();
        let mut reader = MemoryImageImportReader::new(lease, 65536, &input).unwrap();
        assert_eq!(reader.current_read_bytes(), None);
        while reader.advance(64).unwrap() != BackupSourceStep::Unit {}
        assert_eq!(reader.current_unit().unwrap().decoded_bytes, 1);
        assert_eq!(reader.current_read_bytes(), Some(0));
        assert_eq!(
            reader
                .decode_current_step::<()>(|unit, payload| {
                    assert!(matches!(
                        unit.address,
                        DecodedMemoryImageAddress::Catalog(1)
                    ));
                    let mut bytes = Vec::new();
                    payload.read_to_end(&mut bytes)?;
                    assert_eq!(bytes, [1]);
                    Ok(None)
                })
                .unwrap(),
            None
        );
        assert_eq!(reader.current_read_bytes(), Some(1));
        assert!(reader.header().is_some());
        assert!(matches!(
            repository.remove_backup(root.backup(), &control),
            Err(BackupError::Busy)
        ));
        match finish {
            "complete" => {
                for _ in 0..2 {
                    assert_eq!(reader.advance(64).unwrap(), BackupSourceStep::Unit);
                    assert!(matches!(
                        reader.current_unit().unwrap().address,
                        DecodedMemoryImageAddress::Catalog(1)
                    ));
                    assert_eq!(
                        reader.decode_current_step::<()>(|_, _| Ok(None)).unwrap(),
                        None
                    );
                    assert_eq!(reader.current_read_bytes(), Some(1));
                }
                assert_eq!(
                    reader.decode_current_step(|_, _| Ok(Some(17))).unwrap(),
                    Some(17)
                );
                assert_eq!(reader.advance(64).unwrap(), BackupSourceStep::Unit);
                assert!(matches!(
                    reader.current_unit().unwrap().address,
                    DecodedMemoryImageAddress::Catalog(2)
                ));
                assert_eq!(reader.current_read_bytes(), Some(0));
                drop(reader);
                assert_eq!(input.memory().used(), 0);
                continue;
            }
            "zero" => assert!(matches!(reader.advance(0), Err(BackupError::Invalid(_)))),
            "raw" => assert!(matches!(
                reader.read_current(&mut [0]),
                Err(BackupError::Invalid(_))
            )),
            "error" => assert!(matches!(
                reader.decode_current_step::<()>(|_, _| Err(BackupError::Integrity)),
                Err(BackupError::Integrity)
            )),
            "unwind" => assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = reader.decode_current_step::<()>(|_, _| panic!("resumed native codec"));
            }))
            .is_err()),
            "cancel" => {
                input.cancellation().cancel();
                assert!(matches!(
                    reader.decode_current_step::<()>(|_, _| panic!("cancelled codec must not run")),
                    Err(BackupError::Cancelled(_))
                ));
            }
            _ => unreachable!(),
        }
        assert!(matches!(reader.advance(64), Err(BackupError::Finished)));
        assert!(reader.current_unit().is_none());
        assert_eq!(reader.current_read_bytes(), None);
        assert!(reader.header().is_none());
        assert_eq!(input.memory().used(), 0);
    }
}

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
