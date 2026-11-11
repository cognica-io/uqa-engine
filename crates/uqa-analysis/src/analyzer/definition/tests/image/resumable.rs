//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

pub(super) struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("configuration conversion must not reread its source")
    }
}

const WINDOW: ImageDecodeWindow = ImageDecodeWindow { bytes: 1, steps: 1 };

fn configuration() -> Analyzer {
    Analyzer {
        tokenizer: Tokenizer::Pattern {
            pattern: format!("[{}", "한🙂".repeat(4096)),
        },
        token_filters: vec![TokenFilter::Synonym {
            synonyms: (0..64)
                .map(|i| {
                    (
                        format!("{i:04}{}", "x".repeat(4096)),
                        vec!["한글".repeat(100), String::new()],
                    )
                })
                .collect(),
            synonyms_path: Some("unavailable-during-import/synonyms.txt".into()),
        }],
        char_filters: vec![CharFilter::Mapping {
            mapping: [("a".into(), "b".repeat(8192))].into(),
        }],
        normalization: None,
    }
}

#[test]
fn analyzer_conversion_yields_without_source_reads_and_preserves_independent_owners() {
    let bytes = serde_json::to_vec(&configuration()).unwrap();
    let original = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let encoded = admitted(&bytes, &original);
    let pointer = encoded.as_ptr();
    let mut input = ImageDecoder::new(&work, &token, |mut channel| async move {
        AnalyzerDefinition::from_image_from(encoded, channel.input()).await
    })
    .unwrap();
    let mut polls = 0;
    let definition = loop {
        polls += 1;
        if let Some(value) = input.advance(&mut NoRead, WINDOW).unwrap() {
            break value;
        }
    };
    assert!(polls > bytes.len() / 4096);
    assert_eq!(
        serde_json::to_vec(definition.configuration()).unwrap(),
        bytes
    );
    assert_eq!(definition.image.as_ref().unwrap().as_ptr(), pointer);
    assert!(matches!(
        definition.configuration().clone().compile(),
        Err(AnalysisError::InvalidRegex { .. })
    ));
    let capture = MemoryBudget::new(8 << 20);
    let image = definition
        .capture_image(&ProductionControl::new(&capture, &token, &token))
        .unwrap();
    drop((definition, input));
    assert_eq!(work.used(), 0);
    assert!(original.used() > 0);
    assert_eq!(image.as_bytes(), bytes);
    drop(image);
    assert_eq!(original.used(), 0);
    assert_eq!(capture.used(), 0);
}

#[test]
fn analyzer_conversion_retires_incomplete_native_configuration_on_failure() {
    let bytes = serde_json::to_vec(&configuration()).unwrap();
    for mode in ["cancel", "memory", "window", "drop"] {
        let memory = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        let mut input = AnalyzerDefinition::image_input(admitted(&bytes, &memory), &token).unwrap();
        let original = memory.used();
        while memory.used() == original {
            assert!(input.advance(&mut NoRead, WINDOW).unwrap().is_none());
        }
        if mode == "drop" {
            drop(input);
        } else {
            let full = if mode == "memory" {
                Some(memory.reserve(memory.available()).unwrap())
            } else {
                None
            };
            if mode == "cancel" {
                token.cancel();
            }
            let window = ImageDecodeWindow {
                bytes: 1,
                steps: usize::from(mode != "window"),
            };
            let error = loop {
                match input.advance(&mut NoRead, window) {
                    Ok(None) => {}
                    Ok(Some(_)) => panic!("incomplete analyzer published: {mode}"),
                    Err(error) => break error,
                }
            };
            match mode {
                "cancel" => assert!(matches!(
                    error,
                    AnalyzerConfigurationError::Decoding(JsonReadError::Cancelled(_))
                )),
                "memory" => assert!(matches!(
                    error,
                    AnalyzerConfigurationError::Decoding(JsonReadError::Memory(_))
                        | AnalyzerConfigurationError::Admission(ValueRetentionError::Memory(_))
                )),
                _ => assert!(matches!(
                    error,
                    AnalyzerConfigurationError::Decoding(JsonReadError::InvalidJson)
                )),
            }
            assert!(input.advance(&mut NoRead, WINDOW).is_err());
            drop((input, full));
        }
        assert_eq!(memory.used(), 0, "{mode}");
    }
    for invalid in [
        br#"{"tokenizer":{"type":"n_gram","min_gram":99999999999999999999999999999999999999999,"max_gram":2},"token_filters":[],"char_filters":[]}"#.as_slice(),
        br#"{"tokenizer":{"type":"keyword"},"token_filters":[{"type":"synonym","synonyms":{"x":[],"x":[]}}],"char_filters":[]}"#,
        b"{\"tokenizer\":{\"type\":\"pattern\",\"pattern\":\"\xff\"},\"token_filters\":[],\"char_filters\":[]}",
    ] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        assert!(matches!(AnalyzerDefinition::image_input(admitted(invalid, &memory), &token).unwrap().read(&mut NoRead), Err(AnalyzerConfigurationError::Decoding(JsonReadError::InvalidJson))));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn malformed_original_configuration_rejects_skipped_fields_before_payload_admission() {
    let payload = "x".repeat(2 << 20);
    for bytes in [
        format!(
            r#"{{"token_filters":[{{"type":"stop","custom_words":["{payload}"]}}],"char_filters":[]}}"#
        ),
        format!(
            r#"{{"tokenizer":{{"type":"keyword"}},"token_filters":[{{"type":"stop","custom_words":["{payload}"]}}],"char_filters":[]}}"#
        ),
    ] {
        let original = MemoryBudget::new(8 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let encoded = admitted(bytes.as_bytes(), &original);
        let input = ImageDecoder::new(&work, &token, |mut channel| async move {
            AnalyzerDefinition::from_image_from(encoded, channel.input()).await
        })
        .unwrap();
        let result = input.read(&mut NoRead);
        assert!(
            matches!(
                result,
                Err(AnalyzerConfigurationError::Decoding(
                    JsonReadError::InvalidJson
                ))
            ),
            "{result:?}"
        );
        assert_eq!(original.used(), 0);
        assert_eq!(work.used(), 0);
    }
}
