//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn root_matches_independent_wire_fixture() {
    let root = root();
    let fixture = hex_fixture(include_str!("fixtures/root.hex"));
    assert_eq!(root.encode().as_slice(), fixture);
    assert_eq!(BackupRoot::decode(&fixture).unwrap(), root);
    for length in 0..fixture.len() {
        assert!(BackupRoot::decode(&fixture[..length]).is_err());
    }
    let mut oversized = fixture.clone();
    oversized.push(0);
    assert!(BackupRoot::decode(&oversized).is_err());
    for offset in [0, 8, 10, 15, 96, 101, 102, 103] {
        let mut corrupt = fixture.clone();
        corrupt[offset] = 255;
        assert!(BackupRoot::decode(&corrupt).is_err(), "offset {offset}");
    }
}

#[test]
fn incremental_parent_compatibility_is_provider_format_repository_and_incarnation_specific() {
    let parent = root();
    let child = BackupRoot::new(
        parent.repository(),
        BackupId::from_bytes([8; 16]).unwrap(),
        Some(parent.backup()),
        BackupCut {
            snapshot: SnapshotId::from_bytes([9; 16]).unwrap(),
            sequence: Some(CommitSequence::from_u64(12)),
            ..parent.cut()
        },
        span(),
        parent.summary(),
    )
    .unwrap();
    child.validate_parent(parent).unwrap();
    for cut in [
        BackupCut {
            source: DatabaseId::from_bytes([11; 16]),
            ..child.cut()
        },
        BackupCut {
            format: ImageFormat::new(ImageProvider::SQLiteKV, 1).unwrap(),
            ..child.cut()
        },
        BackupCut {
            format: ImageFormat::new(ImageProvider::SQLiteNative, 2).unwrap(),
            ..child.cut()
        },
    ] {
        let incompatible = BackupRoot::new(
            child.repository(),
            child.backup(),
            child.parent(),
            cut,
            span(),
            child.summary(),
        )
        .unwrap();
        assert!(incompatible.validate_parent(parent).is_err());
    }
    let foreign = BackupRoot::new(
        RepositoryId::from_bytes([10; 16]).unwrap(),
        child.backup(),
        child.parent(),
        child.cut(),
        span(),
        child.summary(),
    )
    .unwrap();
    assert!(foreign.validate_parent(parent).is_err());
    assert!(BackupRoot::new(
        parent.repository(),
        parent.backup(),
        Some(parent.backup()),
        parent.cut(),
        span(),
        parent.summary()
    )
    .is_err());
    assert!(parent.validate_parent(child).is_err());
}

#[test]
fn all_image_mappings_and_optional_sequence_round_trip_without_claiming_capabilities() {
    let root = root();
    for provider in [
        ImageProvider::Memory,
        ImageProvider::SQLiteNative,
        ImageProvider::SQLiteKV,
        ImageProvider::Redb,
    ] {
        for sequence in [
            None,
            Some(CommitSequence::INITIAL),
            Some(CommitSequence::from_u64(u64::MAX)),
        ] {
            let cut = BackupCut {
                format: ImageFormat::new(provider, 1).unwrap(),
                sequence,
                ..root.cut()
            };
            let root = BackupRoot::new(
                root.repository(),
                root.backup(),
                None,
                cut,
                span(),
                root.summary(),
            )
            .unwrap();
            assert_eq!(BackupRoot::decode(&root.encode()).unwrap(), root);
        }
    }
    assert_eq!(
        BackupCapabilities::default(),
        BackupCapabilities::UNSUPPORTED
    );
    assert!(ImageFormat::new(ImageProvider::Memory, 0).is_err());
    assert!(BackupLimits::new(0, 1).is_err());
    assert!(BackupLimits::new(1, 0).is_err());
    assert!(RepositoryId::from_bytes([0; 16]).is_err());
}

#[test]
fn object_spans_reject_overflow_zero_frames_and_out_of_range_references() {
    let span = span();
    assert_eq!(ObjectSpan::decode(&span.encode()).unwrap(), span);
    for (object_bytes, offset, encoded_bytes, frames) in [
        (1024, 0, 0, 1),
        (1024, 0, 128, 0),
        (1024, 0, 100, 1),
        (1024, 1000, 128, 1),
        (u64::MAX, u64::MAX - 1, 128, 1),
        (1024, 0, 256, 3),
    ] {
        assert!(ObjectSpan::new(
            span.object(),
            span.digest(),
            object_bytes,
            offset,
            encoded_bytes,
            frames
        )
        .is_err());
    }
    let bytes = span.encode();
    for length in 0..bytes.len() {
        assert!(ObjectSpan::decode(&bytes[..length]).is_err());
    }
}
