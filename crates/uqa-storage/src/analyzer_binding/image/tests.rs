//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::analyzer_binding::capture::{fields, CapturedBoundAnalyzerRevision};
use crate::analyzer_image::AnalyzerImageSnapshot;
use std::collections::BTreeMap;
use uqa_analysis::{Analyzer, AnalyzerLimits, CompiledAnalyzer, Tokenizer};

mod retained;

fn bindings() -> BTreeMap<(String, String), FieldAnalyzerBinding> {
    let index = uqa_analysis::standard_analyzer("english")
        .compile()
        .unwrap();
    let search = Analyzer::new(Tokenizer::Keyword, Vec::new(), Vec::new())
        .compile()
        .unwrap();
    let plain = FieldAnalyzerBinding::unassigned(index.clone(), search.clone());
    BTreeMap::from([
        (("saved".into(), "plain".into()), plain.clone()),
        (
            ("saved".into(), "index".into()),
            plain.assigned(
                "original",
                index.clone(),
                AnalyzerPhase::Index,
                AnalyzerBindingOwner::Field,
            ),
        ),
        (
            ("saved".into(), "search".into()),
            plain
                .assigned(
                    "same",
                    index.clone(),
                    AnalyzerPhase::Index,
                    AnalyzerBindingOwner::Field,
                )
                .assigned(
                    "same",
                    search,
                    AnalyzerPhase::Search,
                    AnalyzerBindingOwner::Field,
                ),
        ),
        (
            ("saved".into(), "gin".into()),
            plain.assigned(
                "원본",
                index,
                AnalyzerPhase::Both,
                AnalyzerBindingOwner::Gin,
            ),
        ),
    ])
}

fn child_bytes(value: &CompiledAnalyzer) -> Vec<u8> {
    assert_eq!(value.dictionary_resources().count(), 0);
    let descriptor = value.descriptor().canonical_json().as_bytes();
    let mut bytes = b"UQAANA01".to_vec();
    bytes.extend((descriptor.len() as u64).to_le_bytes());
    bytes.extend(descriptor);
    bytes.extend(0_u64.to_le_bytes());
    let mut framed = Vec::new();
    for chunk in bytes.chunks(4096) {
        framed.extend((chunk.len() as u32).to_le_bytes());
        framed.extend(chunk);
    }
    framed.extend(0_u32.to_le_bytes());
    framed
}

fn expected(value: &FieldAnalyzerBinding) -> Vec<u8> {
    let mut bytes = b"UQAFAB01".to_vec();
    bytes.push(u8::from(value.owner == AnalyzerBindingOwner::Gin));
    bytes.push(match value.last_phase {
        AnalyzerPhase::Index => 0,
        AnalyzerPhase::Search => 1,
        AnalyzerPhase::Both => 2,
    });
    for side in [&value.index, &value.search] {
        bytes.push(u8::from(side.name.is_some()));
        if let Some(name) = &side.name {
            bytes.extend((name.len() as u64).to_le_bytes());
            bytes.extend(name.as_bytes());
        }
    }
    bytes.extend(child_bytes(&value.index.compiled));
    bytes.extend(child_bytes(&value.search.compiled));
    bytes
}

fn encode(value: &CapturedFieldAnalyzerBinding, window: usize) -> Vec<u8> {
    let control = StorageReadControl::with_limit(8192);
    let mut reader = value.image_reader(&control).unwrap();
    let mut output = Vec::new();
    let mut buffer = vec![0; window];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        assert!(count <= 16 * 1024);
        output.extend_from_slice(&buffer[..count]);
    }
    assert_eq!(control.memory().used(), 0);
    output
}

#[test]
fn field_binding_images_preserve_independent_wire_names_revisions_and_token_observations() {
    let source = bindings();
    let original = StorageReadControl::with_limit(1 << 20);
    let captured = fields(&source, &original).unwrap();
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    for ((_, image), value) in captured.iter().zip(source.values()) {
        let wire = expected(value);
        for window in [1, 7, 4096, 32768] {
            assert_eq!(encode(image, window), wire);
        }
        let input = StorageReadControl::with_limit(1 << 20);
        let restored =
            FieldAnalyzerBinding::read_image(&mut wire.as_slice(), &resources, &input).unwrap();
        assert_eq!(restored.owner, value.owner);
        assert_eq!(restored.last_phase, value.last_phase);
        for (actual, expected) in [
            (&restored.index, &value.index),
            (&restored.search, &value.search),
        ] {
            assert_eq!(actual.name, expected.name);
            assert_eq!(
                actual.compiled.analyze_tokens("The cats and dogs").unwrap(),
                expected
                    .compiled
                    .analyze_tokens("The cats and dogs")
                    .unwrap()
            );
        }
        assert_eq!(expected(&restored), wire);
        drop(restored);
        assert_eq!(input.memory().used(), 0);
    }
    let expected: Vec<_> = source.values().map(expected).collect();
    drop(source);
    for ((_, image), wire) in captured.iter().zip(expected) {
        assert_eq!(encode(image, 11), wire);
    }
    drop(captured);
    assert_eq!(original.memory().used(), 0);
}

#[test]
fn field_binding_images_reject_incomplete_framing_and_inconsistent_assignment() {
    let source = bindings();
    let wire = expected(&source[&("saved".into(), "search".into())]);
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let control = StorageReadControl::with_limit(1 << 20);
    for end in 0..wire.len() {
        assert!(
            FieldAnalyzerBinding::read_image(&mut &wire[..end], &resources, &control).is_err(),
            "prefix {end}"
        );
        assert_eq!(control.memory().used(), 0);
    }
    let mut invalid = Vec::new();
    for position in [0, 8, 9, 10, 23] {
        let mut bytes = wire.clone();
        bytes[position] = 255;
        invalid.push(bytes);
    }
    let mut gin = wire.clone();
    gin[8] = 1;
    invalid.push(gin);
    let mut both = wire.clone();
    both[9] = 2;
    invalid.push(both);
    let mut trailing = wire.clone();
    trailing.push(0);
    invalid.push(trailing);
    let mut frame = wire;
    frame[36..40].copy_from_slice(&4097_u32.to_le_bytes());
    invalid.push(frame);
    for bytes in invalid {
        assert!(
            FieldAnalyzerBinding::read_image(&mut bytes.as_slice(), &resources, &control).is_err()
        );
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn field_binding_images_borrow_wide_names_with_fixed_output_workspace() {
    let mut source = bindings();
    let value = source.get_mut(&("saved".into(), "index".into())).unwrap();
    value.index.name = Some("a".repeat(1 << 20));
    let wire = expected(value);
    let original = StorageReadControl::with_limit(2 << 20);
    let captured = fields(&source, &original).unwrap();
    let image = &captured.iter().find(|(key, _)| key.1 == "index").unwrap().1;
    let output = StorageReadControl::with_limit(8192);
    let mut reader = image.image_reader(&output).unwrap();
    let mut buffer = [0; 4096];
    let mut offset = 0;
    let allocations = allocation_counter::measure(|| loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        assert_eq!(&buffer[..count], &wire[offset..offset + count]);
        offset += count;
    });
    assert_eq!(offset, wire.len());
    assert!(allocations.bytes_total < 16 * 1024, "{allocations:?}");
    assert_eq!(output.memory().used(), 0);
}

struct Interrupted<'a> {
    bytes: &'a [u8],
    control: &'a StorageReadControl,
    read: usize,
    unwind: bool,
}
impl Read for Interrupted<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if self.read > 40 {
            assert!(!self.unwind, "field input unwound after admitting names");
            self.control.cancellation().cancel();
        }
        let count = self.bytes.read(output)?;
        self.read += count;
        Ok(count)
    }
}

#[test]
fn field_binding_images_retain_both_source_controls_and_release_failed_input() {
    let source = bindings();
    let binding = &source[&("saved".into(), "search".into())];
    for cancel_index in [false, true] {
        let index = StorageReadControl::with_limit(1 << 20);
        let search = StorageReadControl::with_limit(1 << 20);
        let captured = CapturedFieldAnalyzerBinding {
            index: CapturedBoundAnalyzerRevision {
                name: binding.index.name.clone(),
                image: AnalyzerImageSnapshot::capture(&binding.index.compiled, &index).unwrap(),
            },
            search: CapturedBoundAnalyzerRevision {
                name: binding.search.name.clone(),
                image: AnalyzerImageSnapshot::capture(&binding.search.compiled, &search).unwrap(),
            },
            owner: binding.owner,
            last_phase: binding.last_phase,
        };
        let output = StorageReadControl::with_limit(8192);
        let mut reader = captured.image_reader(&output).unwrap();
        assert_eq!(reader.read(&mut [0; 1]).unwrap(), 1);
        if cancel_index {
            index.cancellation().cancel();
        } else {
            search.cancellation().cancel();
        }
        assert!(matches!(
            reader.read(&mut [0; 1]),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(output.memory().used(), 0);
        assert!(reader.read(&mut [0; 1]).is_err());
        drop(reader);
        drop(captured);
        assert_eq!(index.memory().used() + search.memory().used(), 0);
    }
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let wire = expected(binding);
    for unwind in [false, true] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = Interrupted {
            bytes: &wire,
            control: &control,
            read: 0,
            unwind,
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            FieldAnalyzerBinding::read_image(&mut input, &resources, &control)
        }));
        if unwind {
            assert!(outcome.is_err());
        } else {
            assert!(matches!(
                outcome.unwrap(),
                Err(StorageBackendError::Cancelled(_))
            ));
        }
        assert_eq!(control.memory().used(), 0);
    }
    let denied = StorageReadControl::with_limit(0);
    assert!(matches!(
        FieldAnalyzerBinding::read_image(&mut wire.as_slice(), &resources, &denied),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(denied.memory().used(), 0);
}
