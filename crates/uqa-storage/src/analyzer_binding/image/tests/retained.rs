//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::Cursor;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

fn destination() -> AnalyzerResources {
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    resources
        .compile(&uqa_analysis::standard_analyzer("english"))
        .unwrap();
    resources
        .compile(&Analyzer::new(Tokenizer::Keyword, Vec::new(), Vec::new()))
        .unwrap();
    resources
}

fn decoder(
    resources: AnalyzerResources,
    control: &StorageReadControl,
) -> ImageDecoder<Budgeted<FieldAnalyzerBinding>, StorageBackendError> {
    ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        |mut channel| async move {
            FieldAnalyzerBinding::read_image_from(&mut channel.input(), &resources).await
        },
    )
    .unwrap()
}

#[test]
fn native_field_binding_input_preserves_both_revisions_names_and_framed_read_windows() {
    let mut bindings = bindings();
    let value = bindings
        .get_mut(&("saved".into(), "search".into()))
        .unwrap();
    value.index.name = Some("원본🙂".repeat(2048));
    value.search.name = Some("search".repeat(4096));
    let wire = expected(value);
    let tokens = [
        value
            .index
            .compiled
            .analyze_tokens("The cats and dogs")
            .unwrap(),
        value
            .search
            .compiled
            .analyze_tokens("The cats and dogs")
            .unwrap(),
    ];
    drop(bindings);
    for width in [1, 17, 4096] {
        let resources = destination();
        let cached = resources.cache_stats().analyzers;
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = decoder(resources.clone(), &control);
        let mut source = Cursor::new(wire.as_slice());
        let restored = loop {
            let before = source.position();
            let value = input
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
        assert_eq!(source.position(), wire.len() as u64);
        assert_eq!(expected(&restored), wire);
        for (side, tokens) in [&restored.index, &restored.search].into_iter().zip(&tokens) {
            assert_eq!(
                side.compiled.analyze_tokens("The cats and dogs").unwrap(),
                *tokens
            );
        }
        assert_eq!(resources.cache_stats().analyzers, cached);
        assert!(!Arc::ptr_eq(
            &restored.index.compiled,
            &resources
                .compile(&uqa_analysis::standard_analyzer("english"))
                .unwrap(),
        ));
        assert_eq!(control.memory().used(), restored.reserved_bytes());
        drop((input, restored));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn native_field_binding_input_retires_names_and_completed_first_child_on_failure() {
    let bindings = bindings();
    let valid = expected(&bindings[&("saved".into(), "search".into())]);
    let resources = destination();
    let cached = resources.cache_stats().analyzers;
    for stop in [16, 128, valid.len() - 1] {
        for action in 0..3 {
            let control = StorageReadControl::with_limit(1 << 20);
            let mut input = decoder(resources.clone(), &control);
            let mut source = Cursor::new(valid.as_slice());
            let window = ImageDecodeWindow { bytes: 1, steps: 1 };
            while source.position() < stop as u64 {
                assert!(input.advance(&mut source, window).unwrap().is_none());
            }
            if action == 0 {
                control.cancellation().cancel();
                assert!(matches!(
                    input.advance(&mut source, window),
                    Err(StorageBackendError::Cancelled(_))
                ));
            } else if action == 1 {
                assert!(input
                    .advance(&mut source, ImageDecodeWindow { bytes: 0, steps: 1 })
                    .is_err());
            }
            drop(input);
            assert_eq!(control.memory().used(), 0);
            assert_eq!(resources.cache_stats().analyzers, cached);
        }
    }
    let control = StorageReadControl::with_limit(1 << 20);
    let mut malformed = valid;
    malformed.pop();
    let input = decoder(resources.clone(), &control);
    assert!(input.read(&mut malformed.as_slice()).is_err());
    assert_eq!(control.memory().used(), 0);
    assert_eq!(resources.cache_stats().analyzers, cached);
}
