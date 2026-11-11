//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{Cursor, Read};
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

fn name(value: &str, control: &StorageReadControl) -> Budgeted<String> {
    ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    )
    .copy_text(value)
    .unwrap()
    .into_budgeted()
    .unwrap()
}

fn wire() -> Vec<u8> {
    let mut words: Vec<_> = (0..256)
        .map(|i| format!("word{i:04}{}", "尾".repeat(128)))
        .collect();
    words.push("skip".into());
    let config = Analyzer::new(
        uqa_analysis::Tokenizer::Whitespace,
        vec![uqa_analysis::TokenFilter::Stop {
            language: String::new(),
            custom_words: words,
        }],
        Vec::new(),
    );
    let source = AnalyzerResources::new(AnalyzerLimits::default())
        .compile(&config)
        .unwrap();
    let mut bytes = Vec::new();
    crate::analyzer_image::write_analyzer_image(
        &source,
        &mut bytes,
        &StorageReadControl::with_limit(0),
    )
    .unwrap();
    bytes
}

struct Source<'a> {
    bytes: Cursor<&'a [u8]>,
    eof: bool,
}
impl Read for Source<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        assert!(
            !self.eof,
            "catalog preparation replayed its completed input"
        );
        let count = self.bytes.read(output)?;
        self.eof = count == 0;
        Ok(count)
    }
}

fn decoder(
    catalog: AnalyzerCatalog,
    name: Budgeted<String>,
    control: &StorageReadControl,
) -> ImageDecoder<AnalyzerCatalog, StorageBackendError> {
    ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        |mut channel| async move {
            catalog
                .read_image_from(
                    name,
                    &AnalyzerResources::new(AnalyzerLimits::default()),
                    &mut channel.input(),
                )
                .await
        },
    )
    .unwrap()
}

#[test]
fn retained_analyzer_catalog_input_preserves_native_roots_names_and_independent_capture() {
    let bytes = wire();
    let label = "saved한🙂".repeat(1024);
    let control = StorageReadControl::with_limit(8 << 20);
    let key = name(&label, &control);
    let pointer = key.as_ptr();
    let mut input = decoder(AnalyzerCatalog::new(control.memory()), key, &control);
    let mut source = Source {
        bytes: Cursor::new(&bytes),
        eof: false,
    };
    let mut calls = 0;
    let mut post_eof = 0;
    let mut catalog = loop {
        calls += 1;
        let width = [1, 37, 4096][calls % 3];
        let before = source.bytes.position();
        post_eof += usize::from(source.eof);
        let value = input
            .advance(
                &mut source,
                ImageDecodeWindow {
                    bytes: width,
                    steps: 3,
                },
            )
            .unwrap();
        assert!(source.bytes.position() - before <= width as u64);
        if let Some(value) = value {
            break value;
        }
        assert!(calls < 100_000);
    };
    assert!(post_eof > 8);
    assert_eq!(catalog.keys().next().unwrap().as_ptr(), pointer);
    assert_eq!(
        catalog.get(&label).unwrap().analyze("keep skip").unwrap(),
        ["keep"]
    );
    let weak = Arc::downgrade(catalog.get(&label).unwrap());
    let capture = StorageReadControl::with_limit(8 << 20);
    let captured = catalog.capture(&capture).unwrap();
    let image = captured.image(&label).unwrap().unwrap();
    let runtime = MemoryBudget::new(8 << 20);
    catalog = catalog.into_mutation_budget(&runtime);
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    catalog
        .insert(&label, uqa_analysis::keyword_analyzer().compile().unwrap())
        .unwrap();
    assert_eq!(
        catalog.get(&label).unwrap().analyze("keep skip").unwrap(),
        ["keep skip"]
    );
    assert!(weak.upgrade().is_none());
    drop((input, catalog, captured, occupied));
    assert_eq!(runtime.used(), 0);
    assert_eq!(image_bytes(&image), bytes);
    drop(image);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(capture.memory().used(), 0);
}

#[test]
fn retained_analyzer_catalog_failure_preserves_retained_roots_and_retires_candidates() {
    let bytes = wire();
    for after_eof in [false, true] {
        for action in 0..3 {
            let control = StorageReadControl::with_limit(8 << 20);
            let mut original = AnalyzerCatalog::new(control.memory());
            original
                .insert(
                    "existing",
                    uqa_analysis::keyword_analyzer().compile().unwrap(),
                )
                .unwrap();
            let used = control.memory().used();
            let mut input = decoder(original.clone(), name("new", &control), &control);
            let mut source = Source {
                bytes: Cursor::new(&bytes),
                eof: false,
            };
            let window = ImageDecodeWindow {
                bytes: 4096,
                steps: 1,
            };
            loop {
                assert!(input.advance(&mut source, window).unwrap().is_none());
                if (after_eof && source.eof) || (!after_eof && source.bytes.position() > 16) {
                    break;
                }
            }
            if action == 0 {
                control.cancellation().cancel();
                assert!(input.advance(&mut source, window).is_err());
            } else if action == 1 {
                assert!(input
                    .advance(&mut source, ImageDecodeWindow { bytes: 0, steps: 1 })
                    .is_err());
            }
            drop(input);
            assert_eq!(original.len(), 1);
            assert_eq!(control.memory().used(), used);
            assert_eq!(
                original.get("existing").unwrap().analyze("a b").unwrap(),
                ["a b"]
            );
            drop(original);
            assert_eq!(control.memory().used(), 0);
        }
    }
    let control = StorageReadControl::with_limit(8 << 20);
    let mut original = AnalyzerCatalog::new(control.memory());
    original
        .insert("saved", Analyzer::default().compile().unwrap())
        .unwrap();
    let used = control.memory().used();
    let input = decoder(original.clone(), name("saved", &control), &control);
    assert!(input.read(&mut bytes.as_slice()).is_err());
    assert_eq!(control.memory().used(), used);
    let foreign = StorageReadControl::with_limit(1024);
    let input = decoder(original.clone(), name("other", &foreign), &control);
    assert!(input.read(&mut bytes.as_slice()).is_err());
    assert_eq!(foreign.memory().used(), 0);
    assert_eq!(control.memory().used(), used);
    drop(original);
    assert_eq!(control.memory().used(), 0);
}
