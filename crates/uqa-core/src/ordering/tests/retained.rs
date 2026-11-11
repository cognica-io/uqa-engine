//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder, ImageIoError},
    CancellationToken,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering as AtomicOrdering},
    Arc,
};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("sorting never reads source bytes")
    }
}

struct Progress<'a> {
    input: AsyncImageInput<'a>,
    comparisons: Arc<AtomicUsize>,
}
impl SortProgress<u32, ImageIoError> for Progress<'_> {
    async fn checkpoint(&mut self) -> Result<(), ImageIoError> {
        self.input.checkpoint().await
    }
    async fn compare(&mut self, left: &u32, right: &u32) -> Result<Ordering, ImageIoError> {
        self.input.checkpoint().await?;
        self.comparisons.fetch_add(1, AtomicOrdering::Relaxed);
        Ok(left.cmp(right))
    }
}

fn input(
    values: Vec<u32>,
    memory: &MemoryBudget,
    token: &CancellationToken,
    comparisons: Arc<AtomicUsize>,
) -> ImageDecoder<Budgeted<Vec<u32>>> {
    let allocation = memory
        .reserve(values.capacity() * size_of::<u32>())
        .unwrap();
    let values = Budgeted::new(values, allocation);
    ImageDecoder::new(memory, token, |mut channel| async move {
        values
            .try_map_async(|mut values| async move {
                sort_by_with_progress(
                    &mut values,
                    &mut Progress {
                        input: channel.input(),
                        comparisons,
                    },
                )
                .await?;
                Ok(values)
            })
            .await
    })
    .unwrap()
}

#[test]
fn retained_ordering_matches_native_order_and_bounds_comparison_progress() {
    for len in [0, 1, 2, 3, 4, 7, 32, 129] {
        for steps in [1, 3, 17] {
            let values: Vec<_> = (0..len).map(|i| (i * 31 + 7) % 19).collect();
            let mut expected = values.clone();
            expected.sort_unstable();
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let comparisons = Arc::new(AtomicUsize::new(0));
            let mut decoder = input(values, &memory, &token, comparisons.clone());
            let output = loop {
                let before = comparisons.load(AtomicOrdering::Relaxed);
                let next = decoder
                    .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })
                    .unwrap();
                assert!(comparisons.load(AtomicOrdering::Relaxed) - before <= steps);
                if let Some(value) = next {
                    break value;
                }
            };
            assert_eq!(*output, expected);
            drop((decoder, output));
            assert_eq!(memory.used(), 0);
            assert_eq!(Arc::strong_count(&comparisons), 1);
        }
    }
}

#[test]
fn retained_ordering_retires_original_elements_on_cancel_invalid_window_and_abandonment() {
    for stop in [0, 1, 2, 7, 64, 511] {
        for action in 0..3 {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let comparisons = Arc::new(AtomicUsize::new(0));
            let mut decoder = input(
                (0..257).rev().collect(),
                &memory,
                &token,
                comparisons.clone(),
            );
            for _ in 0..stop {
                assert!(decoder
                    .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                    .unwrap()
                    .is_none());
            }
            if action != 0 {
                if action == 1 {
                    token.cancel();
                }
                let steps = usize::from(action == 1);
                assert!(decoder
                    .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })
                    .is_err());
                assert_eq!(memory.used(), 0);
                assert_eq!(Arc::strong_count(&comparisons), 1);
                assert!(decoder
                    .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                    .is_err());
            }
            drop(decoder);
            assert_eq!(memory.used(), 0);
            assert_eq!(Arc::strong_count(&comparisons), 1);
        }
    }
}
