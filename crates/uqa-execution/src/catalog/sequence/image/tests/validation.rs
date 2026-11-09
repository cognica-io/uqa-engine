//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn sequence_image_rejects_duplicate_names_objects_and_effective_oids() {
    let mut source = fixture();
    let original = *source
        .catalog
        .get(&RelationIdentity::new("s", "q"))
        .unwrap();
    uqa_core::memory::RetainedValue::make_mut(&mut source.catalog)
        .insert(
            RelationIdentity::new("s", "r"),
            SequenceRecord {
                object_id: [3; 16],
                catalog_oid: Some(31_002),
                ..original
            },
            BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP),
        )
        .unwrap();
    let (metadata, authority) = encode(&source);
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(65536);
    let second = 164 + 18;
    for (range, bytes) in [
        (181..182, vec![b'q']),
        (second..second + 16, vec![2; 16]),
        (second + 17..second + 21, 31_001_u32.to_le_bytes().to_vec()),
    ] {
        let mut changed = metadata.clone();
        changed[range].copy_from_slice(&bytes);
        let error = SequenceCatalog::read_images(
            &mut changed.as_slice(),
            &mut authority.as_slice(),
            &memory,
            &token,
        )
        .err()
        .unwrap();
        assert_eq!(error.sqlstate(), Some("XX001"));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn sequence_image_interruptions_during_input_retire_partial_catalogs() {
    struct Interrupted<'a> {
        bytes: &'a [u8],
        remaining: usize,
        token: &'a CancellationToken,
        io_error: bool,
    }
    impl std::io::Read for Interrupted<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                if self.io_error {
                    return Err(std::io::ErrorKind::PermissionDenied.into());
                }
                self.token.cancel();
            }
            let count = output
                .len()
                .min(self.bytes.len())
                .min(self.remaining.max(1));
            output[..count].copy_from_slice(&self.bytes[..count]);
            self.bytes = &self.bytes[count..];
            self.remaining = self.remaining.saturating_sub(count);
            Ok(count)
        }
    }
    let (metadata, authority) = encode(&fixture());
    for offset in [0, 8, 20, 55, 100, 150] {
        for io_error in [false, true] {
            let memory = MemoryBudget::new(65536);
            let token = CancellationToken::new();
            let mut input = Interrupted {
                bytes: &metadata,
                remaining: offset,
                token: &token,
                io_error,
            };
            let error = SequenceCatalog::read_images(
                &mut input,
                &mut authority.as_slice(),
                &memory,
                &token,
            )
            .err()
            .unwrap();
            if !io_error {
                assert!(matches!(error, SQLError::Cancelled(_)));
            }
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn sequence_image_rejects_truncation_invalid_tags_identities_and_definitions() {
    let (metadata, authority) = encode(&fixture());
    let memory = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let reject = |metadata: &[u8], authority: &[u8]| {
        assert!(
            SequenceCatalog::read_images(&mut &*metadata, &mut &*authority, &memory, &token)
                .is_err()
        );
        assert_eq!(memory.used(), 0);
    };
    for end in 0..metadata.len() {
        reject(&metadata[..end], &authority);
    }
    for end in 0..authority.len() {
        reject(&metadata, &authority[..end]);
    }
    for (offset, value) in [
        (0, b'X'),
        (24, 0),
        (33, 0),
        (34 + 16, 2),
        (34 + 21, 2),
        (34 + 22, 3),
        (34 + 23, 2),
        (34 + 24, 2),
        (34 + 25, 3),
    ] {
        let mut bytes = metadata.clone();
        bytes[offset] = value;
        reject(&bytes, &authority);
    }
    for range in [34..50, 60..76, 76..92, 92..108, 116..124, 156..164] {
        let mut bytes = metadata.clone();
        bytes[range].fill(0);
        reject(&bytes, &authority);
    }
    let mut negative_log = metadata.clone();
    negative_log[132..140].copy_from_slice(&(-1_i64).to_le_bytes());
    reject(&negative_log, &authority);
    let mut trailing = metadata.clone();
    trailing.push(0);
    reject(&trailing, &authority);
    let mut reordered = metadata.clone();
    reordered[8..16].copy_from_slice(&2_u64.to_le_bytes());
    reordered.extend_from_slice(&metadata[16..]);
    reject(&reordered, &authority);
}

#[test]
fn sequence_image_requires_exact_authority_membership_and_persistence() {
    let (metadata, authority) = encode(&fixture());
    let memory = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    for (offset, value) in [(24, b'x'), (33, b'x'), (34, 1)] {
        let mut changed = authority.clone();
        changed[offset] = value;
        assert!(SequenceCatalog::read_images(
            &mut metadata.as_slice(),
            &mut changed.as_slice(),
            &memory,
            &token
        )
        .is_err());
        assert_eq!(memory.used(), 0);
    }
    let empty = b"UQASEQ01\0\0\0\0\0\0\0\0";
    let empty_authority = b"UQASQA01\0\0\0\0\0\0\0\0";
    let restored = SequenceCatalog::read_images(
        &mut empty.as_slice(),
        &mut empty_authority.as_slice(),
        &memory,
        &token,
    )
    .unwrap();
    assert!(restored.is_empty());
    drop(restored);
    assert_eq!(memory.used(), 0);
}

#[test]
fn sequence_image_failure_and_cancellation_release_every_admitted_destination() {
    let (metadata, authority) = encode(&fixture());
    let token = CancellationToken::new();
    let mut success = None;
    for limit in 0..65536 {
        let memory = MemoryBudget::new(limit);
        match SequenceCatalog::read_images(
            &mut metadata.as_slice(),
            &mut authority.as_slice(),
            &memory,
            &token,
        ) {
            Ok(restored) => {
                assert!(memory.used() > 0);
                drop(restored);
                assert_eq!(memory.used(), 0);
                success = Some(limit);
                break;
            }
            Err(error) => {
                assert_eq!(error.sqlstate(), Some("53200"));
                assert_eq!(memory.used(), 0);
            }
        }
    }
    assert!(success.is_some());
    let memory = MemoryBudget::new(65536);
    token.cancel();
    assert!(matches!(
        SequenceCatalog::read_images(
            &mut metadata.as_slice(),
            &mut authority.as_slice(),
            &memory,
            &token
        ),
        Err(SQLError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}

#[test]
fn sequence_image_reader_is_terminal_after_cancellation_write_failure_or_unwind() {
    struct Failed(bool);
    impl Write for Failed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            assert!(!self.0, "writer panic");
            Err(std::io::ErrorKind::WriteZero.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let source = fixture();
    let memory = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let captured =
        durable_sequences(&source, &ProductionControl::new(&memory, &token, &token)).unwrap();
    let cancellation = CancellationToken::new();
    let mut reader = captured.image_reader(&cancellation).unwrap();
    cancellation.cancel();
    assert!(matches!(
        reader.read(&mut [0; 1]),
        Err(SQLError::Cancelled(_))
    ));
    assert_eq!(
        reader.read(&mut [0; 1]).unwrap_err().sqlstate(),
        Some("XX001")
    );
    drop(reader);
    let mut reader = captured.image_reader(&token).unwrap();
    assert!(reader.write_to(&mut Failed(false)).is_err());
    assert_eq!(
        reader.read(&mut [0; 1]).unwrap_err().sqlstate(),
        Some("XX001")
    );
    drop(reader);
    let mut reader = captured.image_reader(&token).unwrap();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || reader.write_to(&mut Failed(true))
    ))
    .is_err());
    assert_eq!(
        reader.read(&mut [0; 1]).unwrap_err().sqlstate(),
        Some("XX001")
    );
    drop(reader);
    drop(captured);
    assert_eq!(memory.used(), 0);
}
