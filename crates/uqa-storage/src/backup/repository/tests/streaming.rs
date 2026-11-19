//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{Read, Seek, SeekFrom, Write};

mod owned;

#[test]
fn backup_image_reader_streams_exact_units_and_bounds_global_verification() {
    let directory = tempfile::tempdir().unwrap();
    let setup = control();
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &setup,
    )
    .unwrap();
    let root = resources::packed_root(&repository, &setup, false).unwrap();
    let control = StorageReadControl::with_limit(128 * 1024);
    let lease = repository.open_backup(root.backup(), &control).unwrap();
    let mut reader = lease.read_image(8 << 20, &control).unwrap();
    let mut units = 0;
    let mut objects = root.inventory().object_bytes();
    loop {
        assert!(reader.verified().is_none());
        let before = reader.verified_object_bytes();
        let step = reader.advance(7).unwrap();
        assert!(reader.verified_object_bytes() - before <= 7);
        match step {
            BackupReadStep::Complete => break,
            BackupReadStep::Pending => continue,
            BackupReadStep::Unit => (),
        }
        let entry = reader.current_unit().unwrap();
        assert_eq!(entry.address().key, (units as u64).to_be_bytes());
        if units == 0 {
            objects += entry.object().object_bytes();
        }
        let mut bytes = 0;
        let mut buffer = [0; 7];
        loop {
            let read = reader.read_current(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            assert_eq!(&buffer[..read], &vec![units as u8; read]);
            bytes += read;
        }
        assert_eq!(bytes, 32);
        assert_eq!(
            reader.verified_object_bytes(),
            0,
            "whole objects are checked after unit streaming"
        );
        units += 1;
    }
    assert_eq!(units, 512);
    assert_eq!(reader.verified_object_bytes(), objects);
    assert_eq!(reader.verified().unwrap().root(), root);
    assert_eq!(control.memory().used(), 0);
    assert!(control.memory().peak() <= 128 * 1024);
    control.cancellation().cancel();
    assert_eq!(reader.advance(0).unwrap(), BackupReadStep::Complete);
    assert!(matches!(
        reader.read_current(&mut [0]),
        Err(BackupError::Finished)
    ));
}

#[test]
fn backup_image_reader_failures_release_every_stream_and_ledger() {
    let directory = tempfile::tempdir().unwrap();
    let setup = control();
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &setup,
    )
    .unwrap();
    let mut operation = repository
        .begin_backup(cut(), None, 1 << 20, &setup)
        .unwrap();
    prepare(&repository, &mut operation, &setup);
    let root = operation.publish(&setup).unwrap();
    let lease = repository.open_backup(root.backup(), &setup).unwrap();
    for failure in 0..6 {
        let control = control();
        let mut reader = lease.read_image(1 << 20, &control).unwrap();
        let result = match failure {
            0 => reader.advance(0).map(|_| ()),
            1 => reader.read_current(&mut [0]).map(|_| ()),
            2 => {
                reader.advance(1).unwrap();
                reader.advance(1).map(|_| ())
            }
            3 => {
                reader.advance(1).unwrap();
                reader.read_current(&mut []).map(|_| ())
            }
            4 => {
                reader.advance(1).unwrap();
                reader.read_current(&mut [0]).unwrap();
                reader.advance(1).map(|_| ())
            }
            _ => {
                reader.advance(1).unwrap();
                while reader.read_current(&mut [0; 32]).unwrap() != 0 {}
                assert_eq!(reader.advance(1).unwrap(), BackupReadStep::Pending);
                control.cancellation().cancel();
                reader.advance(1).map(|_| ())
            }
        };
        assert!(result.is_err());
        assert!(reader.verified().is_none());
        assert_eq!(control.memory().used(), 0);
        assert!(matches!(reader.advance(1), Err(BackupError::Finished)));
    }
}

#[test]
fn backup_image_reader_checks_unreferenced_pack_bytes_before_completion() {
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    let mut operation = repository
        .begin_backup(cut(), None, 1 << 20, &control)
        .unwrap();
    let mut pack = operation.begin_pack(&control).unwrap();
    let selected = pack
        .append(FrameKind::Unit, &mut Cursor::new(b"kept"), &control)
        .unwrap();
    let extra = pack
        .append(FrameKind::Unit, &mut Cursor::new(b"unreferenced"), &control)
        .unwrap();
    let object = pack.finish(&control).unwrap();
    let selected = object.span(selected).unwrap();
    let extra = object.span(extra).unwrap();
    let entry = InventoryEntry::new(
        UnitAddress {
            namespace: NamespaceId::from_bytes([3; 16]).unwrap(),
            generation: 0,
            family: 1,
            key: b"record",
        },
        UnitKind::Record,
        4,
        ImageDigest::of_bytes(b"kept"),
        selected,
        limits(),
        &control,
    )
    .unwrap();
    let mut inventory = InventoryWriter::new(Vec::new(), cut().format, limits(), &control).unwrap();
    inventory.append(&entry).unwrap();
    let (bytes, summary) = inventory.finish().unwrap();
    let span = operation
        .write_object(FrameKind::Inventory, &mut Cursor::new(bytes), &control)
        .unwrap();
    operation.prepare(span, summary, &control).unwrap();
    let root = operation.publish(&control).unwrap();
    let lease = repository.open_backup(root.backup(), &control).unwrap();
    let mut reader = lease.read_image(1 << 20, &control).unwrap();
    reader.advance(1).unwrap();
    let mut bytes = [0; 8];
    assert_eq!(reader.read_current(&mut bytes).unwrap(), 4);
    assert_eq!(&bytes[..4], b"kept");
    assert_eq!(reader.read_current(&mut bytes).unwrap(), 0);
    let name = super::super::directory::name(extra.object().as_bytes());
    let mut file = super::super::directory::open_file(
        &repository.objects,
        super::super::directory::text(&name),
        true,
    )
    .unwrap();
    file.seek(SeekFrom::Start(extra.offset())).unwrap();
    file.write_all(b"!").unwrap();
    let result = loop {
        match reader.advance(3) {
            Ok(BackupReadStep::Pending) => (),
            result => break result,
        }
    };
    assert!(matches!(result, Err(BackupError::Integrity)));
    assert!(reader.verified().is_none());
    drop(entry);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn backup_spans_reject_empty_padding_frames_but_keep_empty_units() {
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    let mut operation = repository
        .begin_backup(cut(), None, 1 << 20, &control)
        .unwrap();
    let mut pack = operation.begin_pack(&control).unwrap();
    let empty = pack
        .append(FrameKind::Unit, &mut Cursor::new(b""), &control)
        .unwrap();
    let data = pack
        .append(FrameKind::Unit, &mut Cursor::new(b"abc"), &control)
        .unwrap();
    let object = pack.finish(&control).unwrap();
    let empty = object.span(empty).unwrap();
    let data = object.span(data).unwrap();
    let combined = ObjectSpan::new(
        empty.object(),
        empty.digest(),
        empty.object_bytes(),
        0,
        empty.encoded_bytes() + data.encoded_bytes(),
        2,
    )
    .unwrap();
    let mut bounded = repository
        .span_reader(combined, FrameKind::Unit, false, &control)
        .unwrap();
    assert!(bounded.read(&mut [0]).is_err());
    let mut empty = repository
        .span_reader(empty, FrameKind::Unit, false, &control)
        .unwrap();
    assert_eq!(empty.read(&mut [0]).unwrap(), 0);
}
