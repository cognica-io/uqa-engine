//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn scoped_native_input_shares_transport_and_keeps_original_payload_admission() {
    let expected = "한λ😀".repeat(257);
    let mut bytes = text_image(expected.as_bytes());
    bytes.extend_from_slice(b"tail");
    for width in [1, 17] {
        let original = MemoryBudget::new(1 << 20);
        let invoking = MemoryBudget::new(1 << 20);
        let original_token = CancellationToken::new();
        let invoking_token = CancellationToken::new();
        let allocation = original.clone();
        let mut decoder = ImageDecoder::<Budgeted<String>>::new(
            &invoking,
            &invoking_token,
            |mut reader| async move {
                let mut input = reader.input();
                let value = input
                    .with_scope(&allocation, &original_token, async |mut child| {
                        child.text().await
                    })
                    .await?;
                assert_eq!(input.array::<4>().await?, *b"tail");
                input.end().await?;
                Ok(value)
            },
        )
        .unwrap();
        let transport_bytes = invoking.used();
        let mut source = Cursor::new(bytes.as_slice());
        let value = loop {
            let before = source.position();
            let result = decoder
                .advance(
                    &mut source,
                    ImageDecodeWindow {
                        bytes: width,
                        steps: 1,
                    },
                )
                .unwrap();
            assert!(source.position() - before <= width as u64);
            if let Some(value) = result {
                break value;
            }
            assert_eq!(invoking.used(), transport_bytes);
        };
        assert_eq!(*value, expected);
        assert!(value.budget().shares_allowance(&original));
        assert_eq!(original.used(), value.reserved_bytes());
        assert_eq!(invoking.used(), 0);
        drop(value);
        assert_eq!(original.used(), 0);
    }
}

#[test]
fn nested_native_input_checks_both_cancellation_owners_before_resumed_child_work() {
    for boundary in 0..32 {
        let mut complete = false;
        for cancel_child in [false, true] {
            let memory = MemoryBudget::new(1 << 20);
            let invoking = CancellationToken::new();
            let original = CancellationToken::new();
            let child_token = original.clone();
            let child_memory = memory.clone();
            let observed = Arc::new(AtomicUsize::new(0));
            let progress = observed.clone();
            let mut decoder =
                ImageDecoder::<usize>::new(&memory, &invoking, |mut reader| async move {
                    let mut input = reader.input();
                    decode_image_from(
                        &mut input,
                        &child_memory,
                        &child_token,
                        async |mut child| {
                            for _ in 0..7 {
                                progress.fetch_add(1, Ordering::SeqCst);
                                child.checkpoint().await?;
                            }
                            Ok(7)
                        },
                    )
                    .await
                })
                .unwrap();
            let window = ImageDecodeWindow { bytes: 1, steps: 1 };
            for _ in 0..boundary {
                if decoder
                    .advance(&mut std::io::empty(), window)
                    .unwrap()
                    .is_some()
                {
                    complete = true;
                    break;
                }
            }
            if !complete {
                let before = observed.load(Ordering::SeqCst);
                if cancel_child {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert!(matches!(
                    decoder.advance(&mut std::io::empty(), window),
                    Err(ImageIoError::Cancelled(_))
                ));
                assert_eq!(
                    observed.load(Ordering::SeqCst),
                    before,
                    "boundary {boundary}, child={cancel_child}"
                );
                assert_eq!(memory.used(), 0);
                assert!(decoder.advance(&mut std::io::empty(), window).is_err());
            }
            drop(decoder);
            assert_eq!(memory.used(), 0);
        }
        if complete {
            assert!(boundary > 7);
            return;
        }
    }
    panic!("nested input did not finish within the fixture bound");
}

#[test]
fn nested_native_input_preserves_parent_windows_and_original_child_payload() {
    let expected = "한λ😀".repeat(2049);
    let mut bytes = text_image(expected.as_bytes());
    bytes.extend_from_slice(b"tail");
    for width in [1, 17, 4096] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut decoder =
            ImageDecoder::<Budgeted<String>>::new(&memory, &token, |mut reader| async move {
                let mut input = reader.input();
                let memory = input.memory;
                let cancellation = input.cancellation;
                let value =
                    decode_image_from(&mut input, memory, cancellation, async |mut child| {
                        child.text().await
                    })
                    .await?;
                assert_eq!(input.array::<4>().await?, *b"tail");
                input.end().await?;
                Ok(value)
            })
            .unwrap();
        let mut source = Cursor::new(bytes.as_slice());
        let value = loop {
            let before = source.position();
            let value = decoder
                .advance(
                    &mut source,
                    ImageDecodeWindow {
                        bytes: width,
                        steps: 3,
                    },
                )
                .unwrap();
            assert!(source.position() - before <= width as u64);
            if let Some(value) = value {
                break value;
            }
        };
        assert_eq!(*value, expected);
        assert_eq!(memory.used(), value.reserved_bytes());
        drop(value);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn nested_native_work_checkpoints_yield_without_reading_the_parent() {
    struct NoRead;
    impl Read for NoRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("child work read parent bytes")
        }
    }
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let observed = Arc::new(AtomicUsize::new(0));
    let progress = observed.clone();
    let mut decoder = ImageDecoder::<()>::new(&memory, &token, |mut reader| async move {
        let mut input = reader.input();
        let memory = input.memory;
        let cancellation = input.cancellation;
        decode_image_from(&mut input, memory, cancellation, async |mut child| {
            for _ in 0..103 {
                child.checkpoint().await?;
                progress.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        })
        .await
    })
    .unwrap();
    loop {
        let before = observed.load(Ordering::SeqCst);
        let complete = decoder
            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 3 })
            .unwrap();
        assert!(observed.load(Ordering::SeqCst) - before <= 3);
        if complete.is_some() {
            break;
        }
    }
    assert_eq!(observed.load(Ordering::SeqCst), 103);
    assert_eq!(memory.used(), 0);
}

#[test]
fn nested_native_input_retires_child_state_on_cancellation_and_denied_payload() {
    for cancel in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut decoder =
            ImageDecoder::<Budgeted<String>>::new(&memory, &token, |mut reader| async move {
                let mut input = reader.input();
                let memory = input.memory;
                let cancellation = input.cancellation;
                decode_image_from(&mut input, memory, cancellation, async |mut child| {
                    child.text().await
                })
                .await
            })
            .unwrap();
        let bytes = text_image(&[b'x'; 100]);
        let mut source = bytes.as_slice();
        assert!(decoder
            .advance(&mut source, ImageDecodeWindow { bytes: 1, steps: 1 })
            .unwrap()
            .is_none());
        let occupied = if cancel {
            token.cancel();
            None
        } else {
            Some(memory.reserve(memory.available()).unwrap())
        };
        let error = decoder.read(&mut source).unwrap_err();
        if cancel {
            assert!(matches!(error, ImageIoError::Cancelled(_)));
        } else {
            assert!(matches!(error, ImageIoError::Memory(_)));
        }
        drop(occupied);
        assert_eq!(memory.used(), 0);
    }
}
