//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
