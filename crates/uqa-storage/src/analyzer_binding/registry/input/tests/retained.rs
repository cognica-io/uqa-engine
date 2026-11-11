//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::Cursor;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

fn decoder(
    catalog: FieldAnalyzerCatalog,
    key: Budgeted<(String, String)>,
    control: &StorageReadControl,
) -> ImageDecoder<FieldAnalyzerCatalog, StorageBackendError> {
    ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        |mut channel| async move {
            catalog
                .read_image_from(
                    key,
                    &AnalyzerResources::new(uqa_analysis::AnalyzerLimits::default()),
                    &mut channel.input(),
                )
                .await
        },
    )
    .unwrap()
}

fn bytes(image: &crate::analyzer_binding::capture::CapturedFieldAnalyzerBinding) -> Vec<u8> {
    let control = StorageReadControl::with_limit(8192);
    let mut source = image.image_reader(&control).unwrap();
    let mut result = Vec::new();
    let mut part = [0; 37];
    loop {
        let count = source.read(&mut part).unwrap();
        if count == 0 {
            return result;
        }
        result.extend_from_slice(&part[..count]);
    }
}

#[test]
fn retained_field_catalog_input_preserves_both_native_roots_through_new_mutations() {
    let wire = wire();
    let field = "body한🙂".repeat(2048);
    let control = StorageReadControl::with_limit(4 << 20);
    let key = key("public.saved", &field, &control);
    let pointer = key.1.as_ptr();
    let mut input = decoder(FieldAnalyzerCatalog::new(control.memory()), key, &control);
    let mut source = Cursor::new(wire.as_slice());
    let mut calls = 0;
    let mut catalog = loop {
        calls += 1;
        let width = [1, 13, 4096][calls % 3];
        let before = source.position();
        let value = input
            .advance(
                &mut source,
                ImageDecodeWindow {
                    bytes: width,
                    steps: 1,
                },
            )
            .unwrap();
        assert!(source.position() - before <= width as u64);
        if let Some(value) = value {
            break value;
        }
        assert!(calls < 100_000);
    };
    let (key, value) = catalog.iter().next().unwrap();
    assert_eq!(key.1.as_ptr(), pointer);
    assert_eq!(value.index.compiled.analyze("a b").unwrap(), ["a", "b"]);
    assert_eq!(value.search.compiled.analyze("a b").unwrap(), ["a b"]);
    let index = Arc::downgrade(&value.index.compiled);
    let search = Arc::downgrade(&value.search.compiled);
    let key = key.clone();
    let capture = StorageReadControl::with_limit(4 << 20);
    let captured = catalog.capture(&capture).unwrap();
    let image = captured.binding(&key).unwrap().unwrap();
    let runtime = MemoryBudget::new(4 << 20);
    catalog = catalog.into_mutation_budget(&runtime);
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    assert!(catalog.remove(&key).unwrap());
    assert!(index.upgrade().is_none());
    assert!(search.upgrade().is_none());
    drop((input, catalog, captured, occupied));
    assert_eq!(bytes(&image), wire);
    drop(image);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(runtime.used(), 0);
    assert_eq!(capture.memory().used(), 0);
}

#[test]
fn retained_field_catalog_failure_retires_both_private_roots_and_preserves_prior_membership() {
    let wire = wire();
    for action in 0..4 {
        let control = StorageReadControl::with_limit(4 << 20);
        let original = FieldAnalyzerCatalog::new(control.memory())
            .insert_image(
                key("public.saved", "existing", &control),
                binding(&wire, &control),
                &control,
            )
            .unwrap();
        let used = control.memory().used();
        let key = key(
            "public.saved",
            if action == 3 { "existing" } else { "new" },
            &control,
        );
        let mut input = decoder(original.clone(), key, &control);
        let mut source = Cursor::new(wire.as_slice());
        let window = ImageDecodeWindow { bytes: 1, steps: 1 };
        while source.position() < wire.len() as u64 {
            assert!(input.advance(&mut source, window).unwrap().is_none());
        }
        if action == 0 {
            control.cancellation().cancel();
            assert!(input.advance(&mut source, window).is_err());
            drop(input);
        } else if action == 1 {
            assert!(input
                .advance(&mut source, ImageDecodeWindow { bytes: 0, steps: 1 })
                .is_err());
            drop(input);
        } else if action == 2 {
            drop(input);
        } else {
            assert!(input.read(&mut source).is_err());
        }
        assert_eq!(original.len(), 1);
        assert_eq!(control.memory().used(), used);
        drop(original);
        assert_eq!(control.memory().used(), 0);
    }
}
