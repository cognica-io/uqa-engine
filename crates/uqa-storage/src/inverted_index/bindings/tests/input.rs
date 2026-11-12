//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::inverted_index::{InvertedIndex, MemoryInvertedIndex, RetainedInvertedIndexBuilder};
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("retained field population must not read source bytes")
    }
}

#[derive(Clone)]
struct Field {
    name: String,
    index: Arc<CompiledAnalyzer>,
    search: Arc<CompiledAnalyzer>,
}

type Selection<'a> = (&'a str, Arc<CompiledAnalyzer>, Arc<CompiledAnalyzer>);

fn selected(field: &Field) -> Selection<'_> {
    (&field.name, field.index.clone(), field.search.clone())
}

fn fields() -> Vec<Field> {
    let resources = uqa_analysis::AnalyzerResources::new(uqa_analysis::AnalyzerLimits::default());
    let index = resources
        .compile(&uqa_analysis::whitespace_analyzer())
        .unwrap();
    let search = resources
        .compile(&uqa_analysis::keyword_analyzer())
        .unwrap();
    (0..12)
        .map(|n| Field {
            name: format!("{n:02}_{}", "한\0🙂".repeat(2048)),
            index: index.clone(),
            search: search.clone(),
        })
        .collect()
}

fn input(
    fields: Vec<Field>,
    control: &StorageReadControl,
) -> ImageDecoder<MemoryInvertedIndex, StorageBackendError> {
    input_with_cancellation(fields, control, control.cancellation())
}

fn input_with_cancellation(
    fields: Vec<Field>,
    control: &StorageReadControl,
    invoking: &uqa_core::CancellationToken,
) -> ImageDecoder<MemoryInvertedIndex, StorageBackendError> {
    let retained = control.clone();
    ImageDecoder::new(control.memory(), invoking, |mut reader| async move {
        let select: fn(&Field) -> crate::StorageBackendResult<Selection<'_>> =
            |field| Ok(selected(field));
        RetainedInvertedIndexBuilder::from_revisions_from(
            AnalyzerBindings::new(uqa_analysis::standard_analyzer("english")).default_binding(),
            fields.iter().map(select),
            &retained,
            &mut reader.input(),
        )
        .await?
        .finish_writable()
    })
    .unwrap()
}

#[test]
fn retained_field_population_resumes_names_and_preserves_exact_historical_revisions() {
    let fields = fields();
    let ordinary_control = StorageReadControl::with_limit(4 << 20);
    let ordinary = RetainedInvertedIndexBuilder::from_revisions(
        AnalyzerBindings::new(uqa_analysis::standard_analyzer("english")).default_binding(),
        fields.iter().map(selected).map(Ok),
        &ordinary_control,
    )
    .unwrap()
    .finish_writable()
    .unwrap();
    let control = StorageReadControl::with_limit(4 << 20);
    let mut input = input(fields.clone(), &control);
    let mut calls = 0;
    let restored = loop {
        calls += 1;
        if let Some(value) = input
            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
            .unwrap()
        {
            break value;
        }
    };
    assert!(calls > fields.len() * 6);
    assert_eq!(control.memory().used(), ordinary_control.memory().used());
    for field in &fields {
        assert!(Arc::ptr_eq(
            &restored.index_analyzer_revision(&field.name).unwrap(),
            &field.index
        ));
        assert!(Arc::ptr_eq(
            &restored.search_analyzer_revision(&field.name).unwrap(),
            &field.search
        ));
    }
    assert!(restored.bindings.default.resolved.get().is_none());
    let mut writable = restored.clone();
    control.cancellation().cancel();
    let full = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    writable
        .set_field_analyzer_revision(
            &fields[0].name,
            fields[0].search.clone(),
            AnalyzerPhase::Both,
        )
        .unwrap();
    assert!(Arc::ptr_eq(
        &restored.index_analyzer_revision(&fields[0].name).unwrap(),
        &fields[0].index
    ));
    assert!(Arc::ptr_eq(
        &writable.index_analyzer_revision(&fields[0].name).unwrap(),
        &fields[0].search
    ));
    drop((input, restored, writable, full, ordinary));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(ordinary_control.memory().used(), 0);
}

#[test]
fn retained_field_population_retires_partial_names_and_revisions_on_failure() {
    for failure in ["cancel", "memory", "window", "drop"] {
        let control = StorageReadControl::with_limit(4 << 20);
        let fields = fields();
        let index = Arc::downgrade(&fields[0].index);
        let search = Arc::downgrade(&fields[0].search);
        let mut input = input(fields, &control);
        for _ in 0..10 {
            assert!(input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
                .is_none());
        }
        let full = (failure == "memory").then(|| {
            control
                .memory()
                .reserve(control.memory().available())
                .unwrap()
        });
        if failure == "cancel" {
            control.cancellation().cancel();
        }
        if failure != "drop" {
            let mut failed = false;
            for _ in 0..128 {
                match input.advance(
                    &mut NoRead,
                    ImageDecodeWindow {
                        bytes: 1,
                        steps: usize::from(failure != "window"),
                    },
                ) {
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("failed field population published: {failure}"),
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
            assert!(failed, "{failure}");
            assert!(input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .is_err());
        }
        drop((input, full));
        assert!(index.upgrade().is_none());
        assert!(search.upgrade().is_none());
        assert_eq!(control.memory().used(), 0, "{failure}");
    }
}

#[test]
fn retained_field_population_checks_both_cancellation_owners_at_every_pending_boundary() {
    let mut fields = fields();
    fields.truncate(2);
    for (i, field) in fields.iter_mut().enumerate() {
        field.name = format!("{i}{}", "한🙂".repeat(800));
    }
    let control = StorageReadControl::with_limit(1 << 20);
    let invoking = uqa_core::CancellationToken::new();
    let window = ImageDecodeWindow { bytes: 1, steps: 1 };
    let mut baseline = input_with_cancellation(fields.clone(), &control, &invoking);
    let mut pending = 0;
    while baseline.advance(&mut NoRead, window).unwrap().is_none() {
        pending += 1;
        assert!(pending < 128);
    }
    drop(baseline);
    assert!(pending > 4);
    assert_eq!(control.memory().used(), 0);
    for stop in 0..=pending {
        for original in [true, false] {
            let control = StorageReadControl::with_limit(1 << 20);
            let invoking = uqa_core::CancellationToken::new();
            let mut input = input_with_cancellation(fields.clone(), &control, &invoking);
            for _ in 0..stop {
                assert!(input.advance(&mut NoRead, window).unwrap().is_none());
            }
            if original {
                control.cancellation().cancel();
            } else {
                invoking.cancel();
            }
            assert!(
                matches!(
                    input.advance(&mut NoRead, window),
                    Err(StorageBackendError::Cancelled(_))
                ),
                "stop {stop}, original {original}"
            );
            assert!(input.advance(&mut NoRead, window).is_err());
            drop(input);
            assert_eq!(
                control.memory().used(),
                0,
                "stop {stop}, original {original}"
            );
        }
    }
}
