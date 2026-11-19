//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{AnalysisData, CompletionMapping, DictionaryLimits, Reader};
use crate::kuromoji::DictionaryError;
use crate::morphology::io::Writer;
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
        panic!("completion preparation must only visit its captured section")
    }
}

struct Fields {
    words: Vec<String>,
    tags: Vec<String>,
    mappings: Vec<CompletionMapping>,
}

fn fields() -> Fields {
    let prefix = "a".repeat(6000);
    Fields {
        words: vec![format!("{prefix}a"), format!("{prefix}b")],
        tags: vec![format!("{prefix}\u{10000}"), format!("{prefix}\u{e000}")],
        mappings: ["a", "b", "\u{10000}", "\u{e000}"]
            .into_iter()
            .map(|suffix| CompletionMapping {
                key: format!("{prefix}{suffix}"),
                alternatives: vec!["same".to_owned(); 300],
            })
            .collect(),
    }
}

fn text(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
    bytes.extend_from_slice(value.as_bytes());
}

fn strings(bytes: &mut Vec<u8>, values: &[String]) {
    bytes.extend_from_slice(&(values.len() as u32).to_le_bytes());
    for value in values {
        text(bytes, value);
    }
}

fn wire() -> Arc<Vec<u8>> {
    let fields = fields();
    let mut bytes = Vec::new();
    strings(&mut bytes, &fields.words);
    strings(&mut bytes, &fields.tags);
    bytes.extend_from_slice(&(fields.mappings.len() as u32).to_le_bytes());
    for mapping in fields.mappings {
        text(&mut bytes, &mapping.key);
        strings(&mut bytes, &mapping.alternatives);
    }
    Arc::new(bytes)
}

fn input(
    bytes: Arc<Vec<u8>>,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<AnalysisData, DictionaryError> {
    ImageDecoder::new(memory, token, |mut channel| async move {
        let mut reader = Reader::new(&bytes, "analysis resources", false);
        let result = AnalysisData::decode_with(
            &mut reader,
            DictionaryLimits {
                max_text_utf16: 6002,
                ..DictionaryLimits::default()
            },
            &mut channel.input(),
        )
        .await?;
        reader.finish()?;
        Ok(result)
    })
    .unwrap()
}

#[test]
fn native_completion_input_preserves_utf16_order_exact_images_and_ranks() {
    let bytes = wire();
    for steps in [1, 13, 512] {
        let measured = allocation_counter::measure(|| {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = input(bytes.clone(), &memory, &token);
            let mut calls = 0;
            let result = loop {
                calls += 1;
                assert!(calls < 100_000);
                if let Some(result) = input
                    .advance(
                        &mut NoRead,
                        ImageDecodeWindow {
                            bytes: calls % 23 + 1,
                            steps,
                        },
                    )
                    .unwrap()
                {
                    break result;
                }
            };
            if steps == 1 {
                assert!(calls > 20_000);
            }
            let expected = fields();
            assert_eq!(result.stop_words, expected.words);
            assert_eq!(result.stop_tags, expected.tags);
            assert_eq!(result.completion.len(), 4);
            assert_eq!(result.completion_lexicon.len(), 4);
            for (rank, (mapping, expected)) in
                result.completion.iter().zip(expected.mappings).enumerate()
            {
                assert_eq!(mapping.key, expected.key);
                assert_eq!(mapping.alternatives, expected.alternatives);
                assert_eq!(
                    result.completion_lexicon.lookup(mapping.key.encode_utf16()),
                    Some(rank as u32)
                );
            }
            assert_eq!(
                result
                    .completion_lexicon
                    .lookup("a".repeat(6000).encode_utf16()),
                None
            );
            let mut output = Writer::default();
            result.encode(&mut output).unwrap();
            assert_eq!(&output.0, bytes.as_ref());
            assert_eq!(memory.used(), 0);
            assert!(input.advance(&mut NoRead, WINDOW).is_err());
        });
        assert_eq!(measured.bytes_current, 0);
    }
}

#[test]
fn native_completion_candidate_interruption_retires_strings_paths_and_interned_states() {
    let bytes = wire();
    for stop in [0, 1, 2, 64, 128, 512, 4096, 20_000] {
        for mode in ["drop", "cancel", "window"] {
            let measured = allocation_counter::measure(|| {
                let memory = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let mut input = input(bytes.clone(), &memory, &token);
                for _ in 0..stop {
                    assert!(input.advance(&mut NoRead, WINDOW).unwrap().is_none());
                }
                match mode {
                    "cancel" => {
                        token.cancel();
                        assert!(matches!(
                            input.advance(&mut NoRead, WINDOW),
                            Err(DictionaryError::Input(ImageIoError::Cancelled(_)))
                        ));
                    }
                    "window" => {
                        assert!(matches!(
                            input.advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 0 }),
                            Err(DictionaryError::Input(ImageIoError::Invalid))
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
            assert_eq!(measured.bytes_current, 0, "stop {stop}, {mode}");
        }
    }
}

fn malformed(case: usize) -> Fields {
    let mut fields = fields();
    match case {
        0 => {
            fields.words.swap(0, 1);
            fields.mappings[0].alternatives.clear();
        }
        1 => {
            fields.mappings.swap(2, 3);
            fields.mappings[0].alternatives.clear();
        }
        2 => fields.mappings[0].alternatives.clear(),
        3 => fields.mappings[3].alternatives[299].clear(),
        4 => (),
        _ => unreachable!(),
    }
    fields
}

#[test]
fn native_completion_candidate_preserves_validation_order_before_length_failure() {
    for case in 0..5 {
        let measured = allocation_counter::measure(|| {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let fields = malformed(case);
            let mut input = ImageDecoder::<_, DictionaryError>::new(
                &memory,
                &token,
                |mut channel| async move {
                    AnalysisData::new_with(
                        fields.words,
                        fields.tags,
                        fields.mappings,
                        1,
                        &mut channel.input(),
                    )
                    .await
                },
            )
            .unwrap();
            let error = loop {
                match input.advance(&mut NoRead, WINDOW) {
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("invalid completion candidate was published"),
                    Err(error) => break error,
                }
            };
            match &error {
                DictionaryError::Invalid {
                    section, reason, ..
                } if case < 4 => {
                    assert_eq!(*section, "analysis resources");
                    assert_eq!(
                        *reason,
                        if case < 2 {
                            "empty, duplicate or unordered key"
                        } else {
                            "completion mapping has no nonempty alternatives"
                        }
                    );
                }
                DictionaryError::Limit {
                    resource,
                    required,
                    limit,
                } if case == 4 => {
                    assert_eq!(*resource, "UTF-16 units per surface");
                    assert_eq!(*required, 6002);
                    assert_eq!(*limit, 1);
                }
                other => panic!("unexpected error ordering: {other}"),
            }
            let fields = malformed(case);
            let ordinary =
                AnalysisData::new(fields.words, fields.tags, fields.mappings, 1).unwrap_err();
            assert_eq!(ordinary.to_string(), error.to_string());
            assert!(input.advance(&mut NoRead, WINDOW).is_err());
            drop(input);
            assert_eq!(memory.used(), 0);
        });
        assert_eq!(measured.bytes_current, 0, "case {case}");
    }
}
