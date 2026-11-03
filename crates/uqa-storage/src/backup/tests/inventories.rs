//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::{self, Seek, Write};

use super::*;

#[test]
fn inventory_matches_independent_fixture_and_completes_only_after_verified_eof() {
    let (bytes, summary) = inventory();
    assert_eq!(bytes, hex_fixture(include_str!("fixtures/inventory.hex")));
    let control = control();
    let mut reader = InventoryReader::new(Cursor::new(bytes), limits(), &control).unwrap();
    assert_eq!(reader.summary(), None);
    assert_eq!(reader.format(), format());
    let entry = reader.read_next().unwrap().unwrap();
    assert_eq!(entry.address(), address(b"alpha"));
    assert_eq!(entry.kind(), UnitKind::Record);
    assert_eq!(entry.decoded_bytes(), 3);
    assert_eq!(entry.digest(), ImageDigest::of_bytes(b"abc"));
    assert_eq!(entry.object(), span());
    assert_eq!(reader.summary(), None);
    assert!(reader.read_next().unwrap().is_none());
    assert_eq!(reader.summary(), Some(summary));
    assert!(reader.read_next().unwrap().is_none());
    drop(reader);
    assert!(control.memory().used() > 0);
    drop(entry);
    assert_eq!(control.memory().used(), 0);
}

fn verify(bytes: &[u8]) -> BackupResult<InventorySummary> {
    let mut reader = InventoryReader::new(Cursor::new(bytes), limits(), &control())?;
    while reader.read_next()?.is_some() {}
    Ok(reader.summary().unwrap())
}

#[test]
fn inventory_rejects_truncation_trailing_bytes_corrupt_digest_and_unknown_fields() {
    let (bytes, _) = inventory();
    for length in 0..bytes.len() {
        assert!(verify(&bytes[..length]).is_err(), "prefix {length}");
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(verify(&trailing).is_err());
    for offset in [0, 8, 10, 11, 12, 16, 41, 52, 53, 61, bytes.len() - 1] {
        let mut corrupt = bytes.clone();
        corrupt[offset] ^= 128;
        assert!(verify(&corrupt).is_err(), "offset {offset}");
    }
}

#[test]
fn protected_inventory_authenticates_object_references_outside_the_logical_image_digest() {
    let (bytes, summary) = inventory();
    let mut relocated = bytes.clone();
    relocated[93] ^= 128;
    assert_eq!(verify(&relocated).unwrap(), summary);
    // The logical image deliberately excludes repository locations. Its enclosing
    // object/frame integrity must still bind those locations before any use.
    for protection in [Protection::Plaintext, protection()] {
        let context = FrameContext {
            kind: FrameKind::Inventory,
            ..context()
        };
        let mut frame = Vec::new();
        write_frame(
            &mut frame,
            context,
            &protection,
            &bytes,
            limits(),
            &control(),
        )
        .unwrap();
        frame[96 + 93] ^= 128;
        assert!(matches!(
            read_frame(
                &mut Cursor::new(frame),
                context,
                &protection,
                limits(),
                &control()
            ),
            Err(BackupError::Integrity)
        ));
    }
}

#[test]
fn duplicate_and_out_of_order_addresses_permanently_fail_the_writer_and_reader() {
    let control = control();
    for next in [b"alpha".as_slice(), b"aardvark"] {
        let mut writer = InventoryWriter::new(Vec::new(), format(), limits(), &control).unwrap();
        writer.append(&entry(b"alpha", &control)).unwrap();
        assert!(matches!(
            writer.append(&entry(next, &control)),
            Err(BackupError::Invalid(_))
        ));
        assert!(matches!(
            writer.append(&entry(b"zulu", &control)),
            Err(BackupError::Finished)
        ));
        assert!(matches!(writer.finish(), Err(BackupError::Finished)));
    }
    let (bytes, _) = inventory();
    let entry_end = bytes.len() - 49;
    let mut repeated = bytes[..entry_end].to_vec();
    repeated.extend_from_slice(&bytes[16..entry_end]);
    repeated.extend_from_slice(&bytes[entry_end..]);
    let mut reader = InventoryReader::new(Cursor::new(repeated), limits(), &control).unwrap();
    assert!(reader.read_next().unwrap().is_some());
    assert!(matches!(reader.read_next(), Err(BackupError::Invalid(_))));
    assert!(matches!(reader.read_next(), Err(BackupError::Finished)));
    assert_eq!(reader.summary(), None);
}

#[test]
fn oversized_keys_are_rejected_before_allocation_or_body_read() {
    let (mut bytes, _) = inventory();
    bytes[43..47].copy_from_slice(&u32::MAX.to_le_bytes());
    let control = StorageReadControl::with_limit(0);
    let mut input = Cursor::new(bytes);
    {
        let mut reader = InventoryReader::new(&mut input, limits(), &control).unwrap();
        assert!(matches!(reader.read_next(), Err(BackupError::Limit { .. })));
        assert!(matches!(reader.read_next(), Err(BackupError::Finished)));
    }
    assert_eq!(input.position(), 47);
    assert_eq!(control.memory().peak(), 0);
}

#[test]
fn ordered_inventory_streams_large_state_under_a_fixed_allowance() {
    let control = StorageReadControl::with_limit(64);
    let mut file = tempfile::tempfile().unwrap();
    let mut writer = InventoryWriter::new(&mut file, format(), limits(), &control).unwrap();
    for id in 0_u64..10000 {
        writer.append(&entry(&id.to_be_bytes(), &control)).unwrap();
    }
    let (_, summary) = writer.finish().unwrap();
    assert_eq!(summary.units, 10000);
    assert_eq!(summary.decoded_bytes, 30000);
    assert_eq!(file.metadata().unwrap().len(), 1_600_065);
    assert_eq!(control.memory().used(), 0);
    file.rewind().unwrap();
    let mut reader = InventoryReader::new(file, limits(), &control).unwrap();
    let mut count = 0_u64;
    while let Some(entry) = reader.read_next().unwrap() {
        assert_eq!(entry.address().key, count.to_be_bytes());
        count += 1;
    }
    assert_eq!(count, 10000);
    assert_eq!(reader.summary(), Some(summary));
    drop(reader);
    assert_eq!(control.memory().used(), 0);
    assert!(control.memory().peak() <= 64);
}

#[test]
fn logical_image_digest_survives_repacking_but_binds_every_semantic_field() {
    let original = inventory().1.image_digest;
    let relocated = ObjectSpan::new(
        ObjectId::from_bytes([8; 16]).unwrap(),
        ImageDigest::of_bytes(b"different protected pack"),
        4096,
        1024,
        512,
        2,
    )
    .unwrap();
    assert_eq!(
        summarize(
            format(),
            address(b"alpha"),
            UnitKind::Record,
            3,
            ImageDigest::of_bytes(b"abc"),
            relocated
        ),
        original
    );
    assert_semantic_fields_bound(original);
}

fn summarize(
    format: ImageFormat,
    address: UnitAddress<'_>,
    kind: UnitKind,
    length: u64,
    digest: ImageDigest,
    object: ObjectSpan,
) -> ImageDigest {
    let control = control();
    let mut writer = InventoryWriter::new(io::sink(), format, limits(), &control).unwrap();
    writer
        .append(
            &InventoryEntry::new(address, kind, length, digest, object, limits(), &control)
                .unwrap(),
        )
        .unwrap();
    writer.finish().unwrap().1.image_digest
}

fn assert_semantic_fields_bound(original: ImageDigest) {
    for changed in [
        summarize(
            ImageFormat::new(ImageProvider::SQLiteKV, 1).unwrap(),
            address(b"alpha"),
            UnitKind::Record,
            3,
            ImageDigest::of_bytes(b"abc"),
            span(),
        ),
        summarize(
            ImageFormat::new(ImageProvider::SQLiteNative, 2).unwrap(),
            address(b"alpha"),
            UnitKind::Record,
            3,
            ImageDigest::of_bytes(b"abc"),
            span(),
        ),
        summarize(
            format(),
            address(b"beta"),
            UnitKind::Record,
            3,
            ImageDigest::of_bytes(b"abc"),
            span(),
        ),
        summarize(
            format(),
            UnitAddress {
                generation: 8,
                ..address(b"alpha")
            },
            UnitKind::Record,
            3,
            ImageDigest::of_bytes(b"abc"),
            span(),
        ),
        summarize(
            format(),
            UnitAddress {
                family: 3,
                ..address(b"alpha")
            },
            UnitKind::Record,
            3,
            ImageDigest::of_bytes(b"abc"),
            span(),
        ),
        summarize(
            format(),
            address(b"alpha"),
            UnitKind::Tombstone,
            3,
            ImageDigest::of_bytes(b"abc"),
            span(),
        ),
        summarize(
            format(),
            address(b"alpha"),
            UnitKind::Record,
            4,
            ImageDigest::of_bytes(b"abc"),
            span(),
        ),
        summarize(
            format(),
            address(b"alpha"),
            UnitKind::Record,
            3,
            ImageDigest::of_bytes(b"abd"),
            span(),
        ),
    ] {
        assert_ne!(changed, original);
    }
}

#[test]
fn unit_lengths_cannot_exceed_the_span_or_decoded_frame_limit() {
    let control = control();
    for length in [145, u64::MAX] {
        assert!(InventoryEntry::new(
            address(b"alpha"),
            UnitKind::Record,
            length,
            ImageDigest::of_bytes(b"abc"),
            span(),
            limits(),
            &control
        )
        .is_err());
    }
    assert!(InventoryEntry::new(
        address(b"alpha"),
        UnitKind::Record,
        3,
        ImageDigest::of_bytes(b"abc"),
        span(),
        BackupLimits::new(100, 2).unwrap(),
        &control
    )
    .is_err());
    let (mut bytes, _) = inventory();
    bytes[53..61].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(
        verify(&bytes),
        Err(BackupError::Invalid("unit length exceeds framed span"))
    ));
    assert_eq!(control.memory().peak(), 0);
}

#[test]
fn writer_admission_enforces_its_own_frame_limit_before_emitting_an_entry() {
    let control = control();
    let entry = entry(b"alpha", &control);
    let smaller = BackupLimits::new(4096, 2).unwrap();
    let mut output = Vec::new();
    let mut writer = InventoryWriter::new(&mut output, format(), smaller, &control).unwrap();
    assert!(matches!(
        writer.append(&entry),
        Err(BackupError::Invalid("unit length exceeds framed span"))
    ));
    assert!(matches!(writer.finish(), Err(BackupError::Finished)));
    assert_eq!(output.len(), 16);
    let mut reader = InventoryReader::new(Cursor::new(output), smaller, &control).unwrap();
    assert!(reader.read_next().is_err());
    assert_eq!(reader.summary(), None);
}

#[test]
fn partial_output_and_cancellation_cannot_be_completed_later() {
    struct Failing {
        remaining: usize,
    }
    impl Write for Failing {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::other("injected output failure"));
            }
            let count = bytes.len().min(self.remaining);
            self.remaining -= count;
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let control = control();
    for allowed in 16..100 {
        let mut writer =
            InventoryWriter::new(Failing { remaining: allowed }, format(), limits(), &control)
                .unwrap();
        assert!(writer.append(&entry(b"alpha", &control)).is_err());
        assert!(matches!(writer.finish(), Err(BackupError::Finished)));
    }
    let mut writer = InventoryWriter::new(io::sink(), format(), limits(), &control).unwrap();
    let entry = entry(b"alpha", &control);
    control.cancellation().cancel();
    assert!(matches!(
        writer.append(&entry),
        Err(BackupError::Cancelled(_))
    ));
    assert!(matches!(writer.finish(), Err(BackupError::Finished)));
}

#[test]
fn empty_inventory_has_a_verified_format_qualified_digest() {
    let control = StorageReadControl::with_limit(0);
    let (bytes, summary) = InventoryWriter::new(Vec::new(), format(), limits(), &control)
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(summary.units, 0);
    assert_eq!(summary.decoded_bytes, 0);
    assert_eq!(verify(&bytes).unwrap(), summary);
    let other = InventoryWriter::new(
        io::sink(),
        ImageFormat::new(ImageProvider::Memory, 1).unwrap(),
        limits(),
        &control,
    )
    .unwrap()
    .finish()
    .unwrap()
    .1;
    assert_ne!(summary.image_digest, other.image_digest);
}
