//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::memory::BudgetedString;

struct NoRead;
impl Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("native text copying must not read source bytes")
    }
}

fn owned(text: &str, memory: &MemoryBudget) -> Budgeted<String> {
    let mut value = BudgetedString::new(memory);
    value.push_str(text).unwrap();
    let (value, reservation) = value.into_parts();
    Budgeted::new(value, reservation)
}

#[test]
fn native_text_copy_yields_per_fragment_without_io_and_preserves_original_ownership() {
    for expected in [String::new(), "aλ한😀".repeat(2049)] {
        for steps in [1_usize, 3, 7] {
            let original = MemoryBudget::new(1 << 20);
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let source = owned(&expected, &original);
            let mut input =
                ImageDecoder::<Budgeted<String>>::new(&memory, &token, |mut reader| async move {
                    reader.input().copy_text(&source).await
                })
                .unwrap();
            let mut calls = 0;
            let value = loop {
                calls += 1;
                if let Some(value) = input
                    .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })
                    .unwrap()
                {
                    break value;
                }
            };
            assert_eq!(calls, (expected.len().div_ceil(CHUNK) + 1).div_ceil(steps));
            assert_eq!(*value, expected);
            assert_eq!(original.used(), 0);
            assert_eq!(memory.used(), value.reserved_bytes());
            drop(value);
            assert_eq!(memory.used(), 0);
        }
        let memory = MemoryBudget::new(expected.len());
        let token = CancellationToken::new();
        let value: Result<_, ImageIoError> =
            decode_image(&mut NoRead, &memory, &token, async |mut input| {
                input.copy_text(&expected).await
            });
        assert_eq!(value.as_ref().unwrap().as_str(), expected);
        drop(value);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_text_copy_retires_source_and_partial_destination_on_cancel_quota_and_drop() {
    for failure in ["cancel", "memory", "drop"] {
        let original = MemoryBudget::new(1 << 20);
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let source = owned(&"한🙂".repeat(2049), &original);
        let mut input =
            ImageDecoder::<Budgeted<String>>::new(&memory, &token, |mut reader| async move {
                reader.input().copy_text(&source).await
            })
            .unwrap();
        let full = (failure == "memory").then(|| memory.reserve(memory.available()).unwrap());
        let window = ImageDecodeWindow { bytes: 1, steps: 1 };
        if failure == "memory" {
            assert!(matches!(
                input.advance(&mut NoRead, window),
                Err(ImageIoError::Memory(_))
            ));
        } else {
            assert!(input.advance(&mut NoRead, window).unwrap().is_none());
            assert!(memory.used() > 0 && original.used() > 0);
            if failure == "cancel" {
                token.cancel();
                assert!(matches!(
                    input.advance(&mut NoRead, window),
                    Err(ImageIoError::Cancelled(_))
                ));
            }
        }
        if failure != "drop" {
            drop(full);
            assert_eq!(original.used(), 0);
            assert_eq!(memory.used(), 0);
            assert!(input.advance(&mut NoRead, window).is_err());
        }
        drop(input);
        assert_eq!(original.used(), 0);
        assert_eq!(memory.used(), 0);
    }
}
