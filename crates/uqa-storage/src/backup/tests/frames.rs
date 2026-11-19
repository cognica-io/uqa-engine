//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::{self, Read};

use super::*;

#[test]
fn plaintext_frame_matches_independent_wire_fixture() {
    let fixture = hex_fixture(include_str!("fixtures/plain-frame.hex"));
    let mut output = Vec::new();
    assert_eq!(
        write_frame(
            &mut output,
            context(),
            &Protection::Plaintext,
            b"abc",
            limits(),
            &control()
        )
        .unwrap(),
        131
    );
    assert_eq!(output, fixture);
    let mut input = Cursor::new(fixture);
    let decoded = read_frame(
        &mut input,
        context(),
        &Protection::Plaintext,
        limits(),
        &control(),
    )
    .unwrap();
    assert_eq!(&*decoded, b"abc");
    assert_eq!(input.position(), 131);
}

#[test]
fn frames_reject_each_corrupted_byte_and_every_truncated_prefix() {
    for protection in [Protection::Plaintext, protection()] {
        let mut output = Vec::new();
        write_frame(
            &mut output,
            context(),
            &protection,
            b"private backup contents",
            limits(),
            &control(),
        )
        .unwrap();
        for offset in 0..output.len() {
            let mut corrupt = output.clone();
            corrupt[offset] ^= 1;
            assert!(
                read_frame(
                    &mut Cursor::new(corrupt),
                    context(),
                    &protection,
                    limits(),
                    &control()
                )
                .is_err(),
                "corruption at {offset}"
            );
            assert!(
                read_frame(
                    &mut Cursor::new(&output[..offset]),
                    context(),
                    &protection,
                    limits(),
                    &control()
                )
                .is_err(),
                "truncation at {offset}"
            );
        }
    }
}

#[test]
fn encrypted_frames_bind_the_repository_domain_object_position_and_kind() {
    let control = control();
    let protection = protection();
    let payload = b"repository-private-source-payload";
    let mut first = Vec::new();
    let mut second = Vec::new();
    write_frame(
        &mut first,
        context(),
        &protection,
        payload,
        limits(),
        &control,
    )
    .unwrap();
    write_frame(
        &mut second,
        context(),
        &protection,
        payload,
        limits(),
        &control,
    )
    .unwrap();
    assert_ne!(first, second);
    assert_ne!(&first[72..96], &second[72..96]);
    assert!(!first.windows(payload.len()).any(|bytes| bytes == payload));
    for context in [
        FrameContext {
            repository: RepositoryId::from_bytes([8; 16]).unwrap(),
            ..context()
        },
        FrameContext {
            object: ObjectId::from_bytes([8; 16]).unwrap(),
            ..context()
        },
        FrameContext {
            offset: context().offset + 1,
            ..context()
        },
        FrameContext {
            kind: FrameKind::Root,
            ..context()
        },
    ] {
        assert!(read_frame(
            &mut Cursor::new(&first),
            context,
            &protection,
            limits(),
            &control
        )
        .is_err());
    }
    for wrong in [
        Protection::Plaintext,
        Protection::Encrypted {
            domain: ProtectionId::from_bytes([8; 16]).unwrap(),
            key: BackupKey::new([42; 32]),
        },
        Protection::Encrypted {
            domain: ProtectionId::from_bytes([9; 16]).unwrap(),
            key: BackupKey::new([43; 32]),
        },
    ] {
        assert!(read_frame(
            &mut Cursor::new(&first),
            context(),
            &wrong,
            limits(),
            &control
        )
        .is_err());
    }
    let decoded = read_frame(
        &mut Cursor::new(first),
        context(),
        &protection,
        limits(),
        &control,
    )
    .unwrap();
    assert_eq!(&*decoded, payload);
    assert_eq!(control.memory().used(), decoded.capacity());
    drop(decoded);
    assert_eq!(control.memory().used(), 0);
    assert!(!format!("{protection:?}").contains("42"));
}

#[test]
fn frame_admission_rejects_lengths_and_allowance_exhaustion_before_payload_reads() {
    let control = StorageReadControl::with_limit(0);
    let mut output = Vec::new();
    assert!(matches!(
        write_frame(
            &mut output,
            context(),
            &protection(),
            b"abc",
            limits(),
            &control
        ),
        Err(BackupError::Memory(_))
    ));
    assert!(output.is_empty());
    let mut fixture = hex_fixture(include_str!("fixtures/plain-frame.hex"));
    let mut input = Cursor::new(&fixture);
    assert!(matches!(
        read_frame(
            &mut input,
            context(),
            &Protection::Plaintext,
            limits(),
            &control
        ),
        Err(BackupError::Memory(_))
    ));
    assert_eq!(input.position(), 96);
    fixture[68..72].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut input = Cursor::new(fixture);
    assert!(matches!(
        read_frame(
            &mut input,
            context(),
            &Protection::Plaintext,
            limits(),
            &control
        ),
        Err(BackupError::Limit { .. })
    ));
    assert_eq!(input.position(), 96);
    assert_eq!(control.memory().peak(), 0);
}

#[test]
fn empty_frames_and_back_to_back_frames_preserve_exact_boundaries() {
    for protection in [Protection::Plaintext, protection()] {
        let control = control();
        let mut output = Vec::new();
        let size =
            write_frame(&mut output, context(), &protection, b"", limits(), &control).unwrap();
        let next = FrameContext {
            offset: context().offset + size,
            ..context()
        };
        write_frame(&mut output, next, &protection, b"next", limits(), &control).unwrap();
        let mut input = Cursor::new(output);
        assert!(
            read_frame(&mut input, context(), &protection, limits(), &control)
                .unwrap()
                .is_empty()
        );
        assert_eq!(input.position(), size);
        assert_eq!(
            &*read_frame(&mut input, next, &protection, limits(), &control).unwrap(),
            b"next"
        );
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn cancellation_survives_interrupted_io_without_retrying_forever() {
    struct Interrupted(crate::read_control::CancellationToken);
    impl Read for Interrupted {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            self.0.cancel();
            Err(io::ErrorKind::Interrupted.into())
        }
    }
    let control = control();
    let mut input = Interrupted(control.cancellation().clone());
    assert!(matches!(
        read_frame(&mut input, context(), &protection(), limits(), &control),
        Err(BackupError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
    let mut output = Vec::new();
    assert!(matches!(
        write_frame(
            &mut output,
            context(),
            &protection(),
            b"",
            limits(),
            &control
        ),
        Err(BackupError::Cancelled(_))
    ));
    assert!(output.is_empty());
}

#[test]
fn frame_offset_overflow_is_rejected_before_output() {
    let mut output = Vec::new();
    let context = FrameContext {
        offset: u64::MAX - 100,
        ..context()
    };
    assert!(write_frame(
        &mut output,
        context,
        &Protection::Plaintext,
        b"",
        limits(),
        &control()
    )
    .is_err());
    assert!(output.is_empty());
}
