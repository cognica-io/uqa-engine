//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Literal archive headers distinguish mapping admission from row materialization.

use super::super::test_image::{repository, Image};
use super::*;
use rusqlite::types::ValueRef;
use uqa_storage::{backup::UnitKind, mvcc::RECLAMATION_EPOCH_NAMESPACE};

#[test]
fn native_backup_import_reader_preserves_literal_namespace_revisions_and_bounded_payloads() {
    let temporary = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let (repository, root) = repository(
        &temporary.path().join("backup"),
        Image::new(vec![71; (512 << 10) + 17]),
        &control,
    );
    let lease = repository.open_backup(root.backup(), &control).unwrap();
    let mut input = SQLiteNativeImageImportReader::new(&lease, 2 << 20, &control).unwrap();
    assert!(input.native_namespace().is_none());
    assert_eq!(input.advance(1).unwrap(), BackupSourceStep::Pending);
    assert_eq!(
        input.native_namespace(),
        Some(DatabaseId::from_bytes([11; 16]))
    );
    let mut units = 0;
    loop {
        match input.advance(4096).unwrap() {
            BackupSourceStep::Pending => continue,
            BackupSourceStep::Complete => break,
            BackupSourceStep::Unit => (),
        }
        match input.current_unit().unwrap() {
            RecordImageImportUnit::Identifier {
                namespace,
                watermark,
            } => {
                assert_eq!(namespace, b"allocator");
                assert_eq!(watermark, 77);
            }
            RecordImageImportUnit::Record {
                revision,
                value_bytes,
                ..
            } => {
                assert_eq!(revision.as_u64(), 7);
                assert_eq!(value_bytes, (512 << 10) + 17);
                let mut bytes = 0;
                let mut output = [0; 127];
                loop {
                    let n = input.read_current(&mut output).unwrap();
                    assert!(output[..n].iter().all(|&byte| byte == 71));
                    bytes += n as u64;
                    if n == 0 {
                        break;
                    }
                }
                assert_eq!(bytes, value_bytes);
            }
        }
        units += 1;
    }
    assert_eq!(units, 2);
    assert_eq!(control.memory().used(), 0);
    assert!(input.native_namespace().is_none());
    assert!(input.current_unit().is_none());
    control.cancellation().cancel();
    assert_eq!(input.advance(0).unwrap(), BackupSourceStep::Complete);
}

#[test]
fn native_backup_import_reader_rejects_incompatible_roots_before_input() {
    for fault in 0..3 {
        let temporary = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(64 << 10);
        let mut image = Image::new(vec![]);
        match fault {
            0 => image.cut.format = ImageFormat::new(ImageProvider::SQLiteKV, 1).unwrap(),
            1 => image.cut.format = ImageFormat::new(ImageProvider::SQLiteNative, 2).unwrap(),
            _ => image.cut.sequence = None,
        }
        let (repository, root) = repository(&temporary.path().join("backup"), image, &control);
        let lease = repository.open_backup(root.backup(), &control).unwrap();
        assert!(matches!(
            SQLiteNativeImageImportReader::new(&lease, 1 << 20, &control),
            Err(BackupError::Unsupported(_))
        ));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn native_backup_import_reader_rejects_verified_but_invalid_mapping_envelopes() {
    for fault in 0..22 {
        let temporary = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(64 << 10);
        let mut image = Image::new(vec![]);
        match fault {
            0 => {
                image.units.remove(0);
            }
            1 => image.units[0].bytes[0] ^= 1,
            2 => image.units[0].bytes[8] ^= 1,
            3 => image.units[0].bytes[31] ^= 1,
            4 => image.units[0].bytes[32..48].fill(0),
            5 => image.units[0].bytes[51] ^= 1,
            6 => image.units[0].bytes[53] ^= 1,
            7 => image.units[0].bytes[55] ^= 1,
            8 => {
                image.units[0].bytes.pop();
            }
            9 => image.units[0].kind = UnitKind::Record,
            10 => image.units[0].key = b"unexpected".to_vec(),
            11 => image.units[1].key.clear(),
            12 => image.units[1].key = RECLAMATION_EPOCH_NAMESPACE.to_vec(),
            13 => image.units[1].kind = UnitKind::Record,
            14 => image.units[2].bytes = 0u64.to_be_bytes().to_vec(),
            15 => image.units[2].bytes = 10u64.to_be_bytes().to_vec(),
            16 => image.units[2].kind = UnitKind::Metadata,
            17 => image.units[2].key = b"not a native key".to_vec(),
            18 => {
                image.units[2].key = NativeRecordIdentity::new(
                    NativeRecordFamily::Metadata,
                    NativeRecordOwner::Database(DatabaseId::from_bytes([12; 16])),
                )
                .unwrap()
                .encode_key(&[ValueRef::Text(b"literal")], &control)
                .unwrap()
                .to_vec();
            }
            19 => {
                image.units[2].key = NativeRecordNamespace(DatabaseId::from_bytes([11; 16]))
                    .key(&control)
                    .unwrap()
                    .to_vec();
            }
            20 => image.units[2].key.push(0),
            _ => image.units.clear(),
        }
        let (repository, root) = repository(&temporary.path().join("backup"), image, &control);
        let lease = repository.open_backup(root.backup(), &control).unwrap();
        let mut input = SQLiteNativeImageImportReader::new(&lease, 1 << 20, &control).unwrap();
        let failed = loop {
            match input.advance(1024) {
                Ok(BackupSourceStep::Pending) => (),
                Ok(BackupSourceStep::Unit) => {
                    if matches!(
                        input.current_unit(),
                        Some(RecordImageImportUnit::Record { .. })
                    ) {
                        while input.read_current(&mut [0; 512]).unwrap() != 0 {}
                    }
                }
                Ok(BackupSourceStep::Complete) => break false,
                Err(_) => break true,
            }
        };
        assert!(failed, "invalid native envelope {fault} was accepted");
        assert!(input.current_unit().is_none() && input.native_namespace().is_none());
        assert!(matches!(input.advance(1), Err(BackupError::Finished)));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn native_backup_import_reader_retires_unread_cancelled_and_exhausted_input() {
    for failure in 0..3 {
        let temporary = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(64 << 10);
        let (repository, root) = repository(
            &temporary.path().join("backup"),
            Image::new(vec![8; 64]),
            &control,
        );
        let lease = repository.open_backup(root.backup(), &control).unwrap();
        let mut input = SQLiteNativeImageImportReader::new(&lease, 1 << 20, &control).unwrap();
        if failure == 0 {
            assert_eq!(input.advance(1024).unwrap(), BackupSourceStep::Pending);
            assert_eq!(input.advance(1024).unwrap(), BackupSourceStep::Unit);
            assert_eq!(input.advance(1024).unwrap(), BackupSourceStep::Unit);
            assert!(input.advance(1024).is_err());
        } else if failure == 1 {
            control.cancellation().cancel();
            assert!(input.advance(1).is_err());
        } else {
            let memory = control
                .memory()
                .reserve((64 << 10) - control.memory().used())
                .unwrap();
            assert!(matches!(input.advance(1), Err(BackupError::Memory(_))));
            drop(memory);
        }
        assert!(input.native_namespace().is_none());
        assert!(matches!(input.advance(1), Err(BackupError::Finished)));
        assert_eq!(control.memory().used(), 0);
    }
}
