//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_analyzer_reconstruction_retires_source_before_canonical_publication() {
    let source = format!(
        r#"{{"token_filters":[{{"synonyms_path":"missing/{}","type":"synonym"}}],"tokenizer":{{"pattern":"[{}","type":"pattern"}}}}"#,
        "한🙂".repeat(4096),
        "한🙂".repeat(4096)
    );
    let expected: Analyzer = serde_json::from_str(&source).unwrap();
    let original = MemoryBudget::new(2 << 20);
    let work = MemoryBudget::new(2 << 20);
    let token = CancellationToken::new();
    let mut input = begin(admitted(&source, &original), &work, &token);
    let mut polls = 0;
    let mut retired_before_complete = false;
    let value = loop {
        polls += 1;
        if let Some(value) = input.advance(&mut NoRead, WINDOW).unwrap() {
            break value;
        }
        if original.used() == 0 {
            retired_before_complete = true;
        }
    };
    assert!(polls > source.len() / 256);
    assert!(retired_before_complete);
    assert_eq!(
        serde_json::to_vec(value.configuration()).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );
    assert!(matches!(
        value.configuration().clone().compile(),
        Err(crate::AnalysisError::InvalidRegex { .. })
    ));
    token.cancel();
    let independent = CancellationToken::new();
    let image = value
        .capture_image(&ProductionControl::new(&work, &independent, &independent))
        .unwrap();
    drop((value, input));
    assert_eq!(image.as_bytes(), serde_json::to_vec(&expected).unwrap());
    drop(image);
    assert_eq!(work.used(), 0);
    assert_eq!(original.used(), 0);
}

#[test]
fn stored_analyzer_reconstruction_releases_all_pending_failures_and_duplicates() {
    let source = r#"{"char_filters":[{"type":"mapping","mapping":{"a":"first","a":"last"}}],"tokenizer":{"pattern":"[","type":"pattern"}}"#;
    interrupted(source);
}

#[cfg(any(feature = "nori", feature = "kuromoji"))]
#[test]
fn stored_analyzer_defaults_keep_admission_at_every_pending_boundary() {
    #[cfg(feature = "nori")]
    interrupted(
        r#"{"tokenizer":{"type":"nori_tokenizer"},"token_filters":[{"type":"unicode_simple_lowercase"}]}"#,
    );
    #[cfg(feature = "kuromoji")]
    interrupted(
        r#"{"tokenizer":{"type":"kuromoji_tokenizer"},"token_filters":[{"type":"kuromoji_completion"},{"type":"unicode_simple_lowercase"}]}"#,
    );
}

fn interrupted(source: &str) {
    let memory = MemoryBudget::new(128 << 10);
    let token = CancellationToken::new();
    let mut complete = begin(admitted(source, &memory), &memory, &token);
    let mut boundaries = 0;
    while complete.advance(&mut NoRead, WINDOW).unwrap().is_none() {
        boundaries += 1;
    }
    drop(complete);
    assert_eq!(memory.used(), 0);
    for stop in 0..boundaries {
        for mode in ["drop", "cancel", "window", "memory"] {
            let token = CancellationToken::new();
            let mut input = begin(admitted(source, &memory), &memory, &token);
            for _ in 0..stop {
                assert!(input.advance(&mut NoRead, WINDOW).unwrap().is_none());
            }
            let occupied = if mode == "memory" {
                Some(memory.reserve(memory.available()).unwrap())
            } else {
                None
            };
            if mode == "cancel" {
                token.cancel();
            }
            if mode != "drop" {
                let window = ImageDecodeWindow {
                    bytes: 1,
                    steps: usize::from(mode != "window"),
                };
                let result = loop {
                    match input.advance(&mut NoRead, window) {
                        Ok(None) => (),
                        Ok(Some(value)) => break Ok(value),
                        Err(error) => break Err(error),
                    }
                };
                match (mode, result) {
                    ("memory", Ok(value)) => assert_eq!(
                        serde_json::to_vec(value.configuration()).unwrap(),
                        serde_json::to_vec(&serde_json::from_str::<Analyzer>(source).unwrap())
                            .unwrap()
                    ),
                    (
                        "memory",
                        Err(
                            AnalyzerConfigurationError::Decoding(JsonReadError::Memory(_))
                            | AnalyzerConfigurationError::Admission(ValueRetentionError::Memory(_)),
                        ),
                    )
                    | (
                        "cancel",
                        Err(AnalyzerConfigurationError::Decoding(JsonReadError::Cancelled(_))),
                    )
                    | (
                        "window",
                        Err(AnalyzerConfigurationError::Decoding(JsonReadError::InvalidJson)),
                    ) => (),
                    (_, value) => panic!("{stop}/{mode}: {value:?}"),
                }
                assert!(input.advance(&mut NoRead, WINDOW).is_err());
            }
            drop((input, occupied));
            assert_eq!(memory.used(), 0, "{stop}/{mode}");
        }
    }
}

#[test]
fn stored_analyzer_input_yields_before_wide_map_payload_and_reuses_source_text() {
    let source = format!(
        r#"{{"char_filters":[{{"type":"mapping","mapping":{{{}}}}}]}}"#,
        (0..256)
            .map(|i| format!(r#""{i:04}{}":"{}""#, "한🙂".repeat(32), "x".repeat(4096)))
            .collect::<Vec<_>>()
            .join(",")
    );
    let original = MemoryBudget::new(4 << 20);
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let mut input = begin(admitted(&source, &original), &memory, &token);
    let mut polls = 0;
    // Inspect native reconstruction before its single, separately admitted output allocation.
    while original.used() != 0 {
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = input.advance(&mut NoRead, WINDOW).unwrap();
        });
        assert!(result.is_none());
        assert!(
            allocation.bytes_total < 64 << 10,
            "poll {polls}: {allocation:?}"
        );
        polls += 1;
    }
    assert!(polls > 256);
    let value = input.read(&mut NoRead).unwrap();
    assert_eq!(
        serde_json::to_vec(value.configuration()).unwrap(),
        serde_json::to_vec(&serde_json::from_str::<Analyzer>(&source).unwrap()).unwrap()
    );
    drop(value);
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_analyzer_map_replacement_retires_discarded_native_payload_charges() {
    let source = format!(
        r#"{{"char_filters":[{{"type":"mapping","mapping":{{"a":"{}","a":"last"}}}}]}}"#,
        "x".repeat(1 << 18)
    );
    let original = MemoryBudget::new(1 << 20);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let value = begin(admitted(&source, &original), &memory, &token)
        .read(&mut NoRead)
        .unwrap();
    assert_eq!(original.used(), 0);
    assert!(
        memory.used() < 4096,
        "discarded native value still retained {} bytes",
        memory.used()
    );
    let CharFilter::Mapping { mapping } = &value.configuration().char_filters[0] else {
        panic!("native mapping")
    };
    assert_eq!(mapping.get("a").unwrap(), "last");
    drop(value);
    assert_eq!(memory.used(), 0);
}
