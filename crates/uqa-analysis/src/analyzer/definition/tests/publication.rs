//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{CharFilter, TokenFilter, Tokenizer};
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

const WINDOW: ImageDecodeWindow = ImageDecodeWindow { bytes: 1, steps: 1 };

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("native configuration publication does not read an external source")
    }
}

fn admitted(configuration: &Analyzer, memory: &MemoryBudget) -> Budgeted<Analyzer> {
    let bytes = serde_json::to_vec(configuration).unwrap();
    super::super::image::decode(&bytes, memory, &CancellationToken::new()).unwrap()
}

fn begin(
    configuration: Budgeted<Analyzer>,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<AnalyzerDefinition, AnalyzerConfigurationError> {
    ImageDecoder::new(memory, token, |mut channel| async move {
        AnalyzerDefinition::from_configuration_from(configuration, channel.input()).await
    })
    .unwrap()
}

pub(super) fn check_bytes(configuration: &Analyzer) {
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let value = begin(admitted(configuration, &memory), &memory, &token)
        .read(&mut NoRead)
        .unwrap();
    assert_eq!(
        value.image.as_ref().unwrap().as_bytes(),
        serde_json::to_vec(configuration).unwrap()
    );
    assert_eq!(
        serde_json::to_vec(value.configuration()).unwrap(),
        serde_json::to_vec(configuration).unwrap()
    );
    drop(value);
    assert_eq!(memory.used(), 0);
}

fn configuration() -> Analyzer {
    Analyzer {
        tokenizer: Tokenizer::Pattern {
            pattern: format!("[{}", "한🙂\"\\\n\u{0}\u{8}\u{c}".repeat(1024)),
        },
        token_filters: vec![TokenFilter::Synonym {
            synonyms: [(
                "한🙂".repeat(1024),
                vec!["\u{1}".repeat(1024), String::new()],
            )]
            .into(),
            synonyms_path: Some(format!("missing/{}", "한🙂".repeat(1024)).into()),
        }],
        char_filters: vec![CharFilter::Mapping {
            mapping: [("z".into(), "🙂".repeat(1024))].into(),
        }],
        normalization: None,
    }
}

#[test]
fn native_analyzer_publication_yields_and_separates_configuration_and_image_owners() {
    let source = configuration();
    let expected = serde_json::to_vec(&source).unwrap();
    let original = MemoryBudget::new(1 << 20);
    let output = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let configuration = admitted(&source, &original);
    let Tokenizer::Pattern { pattern } = &configuration.tokenizer else {
        unreachable!()
    };
    let pointer = pattern.as_ptr();
    let mut input = begin(configuration, &output, &token);
    let mut polls = 0;
    let definition = loop {
        polls += 1;
        if let Some(value) = input.advance(&mut NoRead, WINDOW).unwrap() {
            break value;
        }
    };
    assert!(polls > expected.len() / 256);
    assert_eq!(definition.image.as_ref().unwrap().as_bytes(), expected);
    let Tokenizer::Pattern { pattern } = &definition.configuration().tokenizer else {
        unreachable!()
    };
    assert_eq!(pattern.as_ptr(), pointer);
    assert!(matches!(
        definition.configuration().clone().compile(),
        Err(AnalysisError::InvalidRegex { .. })
    ));
    let retained = definition.clone();
    let capture = MemoryBudget::new(1 << 20);
    let image = definition
        .capture_image(&ProductionControl::new(&capture, &token, &token))
        .unwrap();
    drop((definition, input));
    assert!(original.used() > 0);
    std::thread::spawn(move || drop(retained)).join().unwrap();
    assert_eq!(original.used(), 0);
    assert_eq!(
        output.used(),
        image.as_bytes().len() + size_of::<Produced<Vec<u8>>>()
    );
    assert_eq!(image.as_bytes(), expected);
    drop(image);
    assert_eq!(output.used(), 0);
    assert_eq!(capture.used(), 0);
}

#[test]
fn native_analyzer_publication_retires_every_interrupted_continuation() {
    let source = Analyzer {
        tokenizer: Tokenizer::Pattern {
            pattern: "한🙂".repeat(80),
        },
        ..Analyzer::default()
    };
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let mut full = begin(admitted(&source, &memory), &memory, &token);
    let mut boundaries = 0;
    while full.advance(&mut NoRead, WINDOW).unwrap().is_none() {
        boundaries += 1;
    }
    drop(full);
    assert_eq!(memory.used(), 0);
    let mut limited = 0;
    let mut reclaimed = 0;
    for boundary in 0..boundaries {
        for failure in ["cancel", "memory", "window", "drop"] {
            let token = CancellationToken::new();
            let mut input = begin(admitted(&source, &memory), &memory, &token);
            for _ in 0..boundary {
                assert!(input.advance(&mut NoRead, WINDOW).unwrap().is_none());
            }
            let occupied = if failure == "memory" {
                Some(memory.reserve(memory.available()).unwrap())
            } else {
                None
            };
            if failure == "cancel" {
                token.cancel();
            }
            if failure != "drop" {
                let window = ImageDecodeWindow {
                    bytes: 1,
                    steps: usize::from(failure != "window"),
                };
                let result = loop {
                    match input.advance(&mut NoRead, window) {
                        Ok(None) => (),
                        Ok(Some(value)) => break Ok(value),
                        Err(error) => break Err(error),
                    }
                };
                match (failure, result) {
                    (
                        "cancel",
                        Err(AnalyzerConfigurationError::Decoding(JsonReadError::Cancelled(_))),
                    )
                    | (
                        "window",
                        Err(AnalyzerConfigurationError::Decoding(JsonReadError::InvalidJson)),
                    ) => (),
                    (
                        "memory",
                        Err(AnalyzerConfigurationError::Admission(ValueRetentionError::Memory(_))),
                    ) => limited += 1,
                    ("memory", Ok(value)) => {
                        // Retired traversal buffers can fund the final shared headers.
                        assert_eq!(
                            value.image.as_ref().unwrap().as_bytes(),
                            serde_json::to_vec(&source).unwrap()
                        );
                        assert!(memory.peak() <= memory.limit());
                        reclaimed += 1;
                    }
                    (_, result) => panic!("unexpected outcome: {boundary}/{failure}: {result:?}"),
                }
                assert!(input.advance(&mut NoRead, WINDOW).is_err());
            }
            drop((input, occupied));
            assert_eq!(memory.used(), 0, "{boundary}/{failure}");
        }
    }
    assert!(limited > 0 && reclaimed > 0);
}

#[test]
fn native_analyzer_publication_allocates_only_output_and_fixed_traversal() {
    for count in [32, 4096] {
        let source = Analyzer {
            token_filters: vec![TokenFilter::Synonym {
                synonyms: (0..count)
                    .map(|i| (format!("{i:08}한🙂"), vec!["value\n".repeat(8)]))
                    .collect(),
                synonyms_path: None,
            }],
            ..Analyzer::default()
        };
        let expected = serde_json::to_vec(&source).unwrap();
        let original = MemoryBudget::new(8 << 20);
        let output = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        let input = begin(admitted(&source, &original), &output, &token);
        let mut definition = None;
        let allocations = allocation_counter::measure(|| {
            definition = Some(input.read(&mut NoRead).unwrap());
        });
        // The exact output buffer is allocated once; no field-sized scratch or
        // population-sized JSON tree is allocated during either traversal.
        assert!(
            allocations.bytes_total < u64::try_from(expected.len() + (16 << 10)).unwrap(),
            "{count}: {allocations:?}"
        );
        let definition = definition.unwrap();
        assert_eq!(definition.image.as_ref().unwrap().as_bytes(), expected);
        assert_eq!(
            output.used(),
            expected.len() + size_of::<Produced<Vec<u8>>>()
        );
        drop(definition);
        assert_eq!(original.used(), 0);
        assert_eq!(output.used(), 0);
    }
}

#[cfg(unix)]
#[test]
fn native_analyzer_publication_rejects_invalid_path_bytes_without_panic_or_leak() {
    use std::os::unix::ffi::OsStringExt;
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    for position in [0, 255, 256, 257, 1024] {
        let mut path = vec![b'x'; position];
        path.push(0xff);
        let configuration = Analyzer {
            token_filters: vec![TokenFilter::Synonym {
                synonyms: std::collections::BTreeMap::new(),
                synonyms_path: Some(std::ffi::OsString::from_vec(path).into()),
            }],
            ..Analyzer::default()
        };
        let expected = serde_json::to_vec(&configuration).unwrap_err().to_string();
        // Invalid paths have no JSON representation; admit the fixture's native owners directly.
        let TokenFilter::Synonym {
            synonyms_path: Some(path),
            ..
        } = &configuration.token_filters[0]
        else {
            unreachable!()
        };
        let bytes =
            configuration.token_filters.capacity() * size_of::<TokenFilter>() + path.capacity();
        let configuration = Budgeted::new(configuration, memory.reserve(bytes).unwrap());
        let error = begin(configuration, &memory, &token)
            .read(&mut NoRead)
            .unwrap_err();
        let AnalyzerConfigurationError::Encoding(error) = error else {
            panic!("expected path encoding error")
        };
        let AnalysisError::Json(error) = error.as_ref() else {
            panic!("expected JSON diagnostic")
        };
        assert_eq!(error.to_string(), expected);
        assert_eq!(memory.used(), 0);
    }
}
