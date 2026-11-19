//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::AnalysisError;
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder, ImageIoError},
    memory::MemoryBudget,
    CancellationToken,
};

const WINDOW: ImageDecodeWindow = ImageDecodeWindow { bytes: 1, steps: 1 };

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("configuration materialization borrows its original decoded value")
    }
}

fn fixture(count: usize) -> Arc<Value> {
    Arc::new(
        serde_json::to_value(Analyzer::new(
            Tokenizer::Pattern {
                pattern: "uncompiled[".into(),
            },
            vec![TokenFilter::Synonym {
                synonyms: (0..count)
                    .map(|i| {
                        (
                            format!("{i:04}{}", "한🙂".repeat(128)),
                            vec!["value".repeat(819), "尾".repeat(1024)],
                        )
                    })
                    .collect(),
                synonyms_path: Some("absent-during-restoration".into()),
            }],
            vec![CharFilter::Mapping {
                mapping: [("source".into(), "target".repeat(2048))].into(),
            }],
        ))
        .unwrap(),
    )
}

fn open(
    value: Arc<Value>,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<Analyzer, AnalysisError> {
    ImageDecoder::new(memory, token, |mut channel| async move {
        decode(&value, &mut channel.input()).await
    })
    .unwrap()
}

#[test]
fn native_configuration_construction_yields_without_complete_value_copies() {
    let value = fixture(256);
    let expected: Analyzer = serde_json::from_value(value.as_ref().clone()).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = open(Arc::clone(&value), &memory, &token);
    let mut calls = 0;
    let actual = loop {
        let mut completed = None;
        let allocation = allocation_counter::measure(|| {
            completed = input.advance(&mut NoRead, WINDOW).unwrap();
        });
        assert!(
            allocation.bytes_total < 64 * 1024,
            "configuration poll {calls} allocated {} bytes",
            allocation.bytes_total
        );
        calls += 1;
        if let Some(value) = completed {
            break value;
        }
        assert!(calls < 100_000);
    };
    assert!(calls > 1024);
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert!(input.advance(&mut NoRead, WINDOW).is_err());
    drop(input);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_configuration_construction_retires_partial_fields_and_original_borrows() {
    let value = fixture(4);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = open(Arc::clone(&value), &memory, &token);
    let mut calls = 0;
    loop {
        calls += 1;
        if input.advance(&mut NoRead, WINDOW).unwrap().is_some() {
            break;
        }
    }
    drop(input);
    for stop in 0..calls {
        for mode in ["drop", "cancel", "window"] {
            let allocation = allocation_counter::measure(|| {
                let memory = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let mut input = open(Arc::clone(&value), &memory, &token);
                for _ in 0..stop {
                    assert!(input.advance(&mut NoRead, WINDOW).unwrap().is_none());
                }
                match mode {
                    "cancel" => {
                        token.cancel();
                        assert!(matches!(
                            input.advance(&mut NoRead, WINDOW),
                            Err(AnalysisError::Input(ImageIoError::Cancelled(_)))
                        ));
                    }
                    "window" => {
                        assert!(matches!(
                            input.advance(&mut NoRead, ImageDecodeWindow { bytes: 0, steps: 1 }),
                            Err(AnalysisError::Input(ImageIoError::Invalid))
                        ));
                    }
                    _ => (),
                }
                if mode != "drop" {
                    assert!(input.advance(&mut NoRead, WINDOW).is_err());
                }
                drop(input);
                assert_eq!(memory.used(), 0);
            });
            assert_eq!(allocation.bytes_current, 0, "{mode} at {stop}");
            assert_eq!(Arc::strong_count(&value), 1);
        }
    }
}
