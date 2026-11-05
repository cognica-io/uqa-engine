//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    analyzer_image::{read_analyzer_image, write_analyzer_image},
    StorageBackendError,
};
use uqa_analysis::{Analyzer, AnalyzerLimits, AnalyzerResourceKind, AnalyzerResources};

fn encode(source: &CompiledAnalyzer) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_analyzer_image(source, &mut bytes, &StorageReadControl::with_limit(0)).unwrap();
    bytes
}

fn saved_bytes(source: &AnalyzerImageSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    source
        .write_image(&mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    bytes
}

#[test]
fn analyzer_capture_image_preserves_bytes_after_source_loss_and_last_owner_retirement() {
    let owner = AnalyzerResources::new(AnalyzerLimits::default());
    let source = owner
        .compile(&uqa_analysis::standard_analyzer("english"))
        .unwrap();
    let weak = Arc::downgrade(&source);
    let expected = encode(&source);
    let control = StorageReadControl::with_limit(1 << 20);
    let image = AnalyzerImageSnapshot::capture(&source, &control).unwrap();
    let other_control = StorageReadControl::with_limit(1 << 20);
    let other = AnalyzerImageSnapshot::capture(&source, &other_control).unwrap();
    let clone = image.clone();
    drop((source, image, owner));
    assert!(weak.upgrade().is_none());
    assert_eq!(saved_bytes(&clone), expected);
    assert_eq!(saved_bytes(&other), expected);
    assert_eq!(control.memory().used(), other_control.memory().used());
    drop(clone);
    assert_eq!(control.memory().used(), 0);
    assert!(other_control.memory().used() > 0);
    drop(other);
    assert_eq!(other_control.memory().used(), 0);
}

#[test]
fn analyzer_capture_image_retains_original_and_invoking_cancellation_between_short_writes() {
    struct ShortWriter {
        cancellation: uqa_core::CancellationToken,
        calls: usize,
    }
    impl Write for ShortWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls == 4 {
                self.cancellation.cancel();
            }
            Ok(bytes.len().min(3))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let source = Analyzer::default().compile().unwrap();
    let small = StorageReadControl::with_limit(0);
    assert!(matches!(
        AnalyzerImageSnapshot::capture(&source, &small),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(small.memory().used(), 0);
    for original in [false, true] {
        let control = StorageReadControl::with_limit(1 << 20);
        let invoking = StorageReadControl::with_limit(0);
        let image = AnalyzerImageSnapshot::capture(&source, &control).unwrap();
        let mut writer = ShortWriter {
            cancellation: if original {
                control.cancellation().clone()
            } else {
                invoking.cancellation().clone()
            },
            calls: 0,
        };
        assert!(matches!(
            image.write_image(&mut writer, &invoking),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(writer.calls, 4);
        if original {
            let mut output = Vec::new();
            assert!(matches!(
                image.write_image(&mut output, &StorageReadControl::with_limit(0)),
                Err(StorageBackendError::Cancelled(_))
            ));
            assert!(output.is_empty());
        } else {
            assert_eq!(saved_bytes(&image), encode(&source));
        }
        drop(image);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn analyzer_capture_image_releases_imported_dictionary_owners_before_snapshot_retirement() {
    for (kind, tokenizer, query) in [
        (
            AnalyzerResourceKind::Nori,
            "nori_tokenizer",
            "서울에서 학교에 갔다",
        ),
        (
            AnalyzerResourceKind::Kuromoji,
            "kuromoji_tokenizer",
            "東京都で食べた",
        ),
    ] {
        let owner = AnalyzerResources::new(AnalyzerLimits::default());
        if owner.image_resource_byte_limit(kind).is_err() {
            continue;
        }
        let config: Analyzer = serde_json::from_value(serde_json::json!({
            "tokenizer": {"type": tokenizer}, "normalization": {"type": "unavailable"}
        }))
        .unwrap();
        let source = owner.compile(&config).unwrap();
        let expected = source.analyze_tokens(query).unwrap();
        let bytes = encode(&source);
        drop(source);
        let imported = StorageReadControl::with_limit(128 << 20);
        let source = read_analyzer_image(&mut bytes.as_slice(), &owner, &imported).unwrap();
        assert!(imported.memory().used() > 0);
        let control = StorageReadControl::with_limit(128 << 20);
        let image = AnalyzerImageSnapshot::capture(&source, &control).unwrap();
        drop((source, owner));
        assert_eq!(imported.memory().used(), 0);
        assert_eq!(saved_bytes(&image), bytes);
        let restored_control = StorageReadControl::with_limit(128 << 20);
        let restored_owner = AnalyzerResources::default();
        let restored = read_analyzer_image(
            &mut saved_bytes(&image).as_slice(),
            &restored_owner,
            &restored_control,
        )
        .unwrap();
        assert_eq!(restored.analyze_tokens(query).unwrap(), expected);
        drop((restored, restored_owner, image));
        assert_eq!(control.memory().used(), 0);
        assert_eq!(restored_control.memory().used(), 0);
    }
}

#[test]
fn native_analyzer_images_release_import_allowances_and_preserve_enabled_analysis() {
    for (kind, tokenizer, query) in [
        (
            AnalyzerResourceKind::Nori,
            "nori_tokenizer",
            "서울에서 학교에 갔다",
        ),
        (
            AnalyzerResourceKind::Kuromoji,
            "kuromoji_tokenizer",
            "東京都で食べた",
        ),
    ] {
        let owner = AnalyzerResources::new(AnalyzerLimits::default());
        if owner.image_resource_byte_limit(kind).is_err() {
            continue;
        }
        let configuration: Analyzer = serde_json::from_value(serde_json::json!({
            "tokenizer": {"type": tokenizer}, "normalization": {"type": "unavailable"}
        }))
        .unwrap();
        let compiled = owner.compile(&configuration).unwrap();
        let expected = compiled.analyze_tokens(query).unwrap();
        let bytes = encode(&compiled);
        drop((compiled, owner));
        let owner = AnalyzerResources::new(AnalyzerLimits::default());
        let imported = StorageReadControl::with_limit(128 << 20);
        let compiled = read_analyzer_image(&mut bytes.as_slice(), &owner, &imported).unwrap();
        assert!(imported.memory().used() > 0);
        let native_memory = uqa_core::memory::MemoryBudget::new(128 << 20);
        let cancellation = uqa_core::CancellationToken::new();
        let production = ProductionControl::new(&native_memory, &cancellation, &cancellation);
        let source = compiled.image_source_with_control(&production).unwrap();
        let capture = StorageReadControl::with_limit(128 << 20);
        let mut image = None;
        let measured = allocation_counter::measure(|| {
            image = Some(AnalyzerImageSnapshot::capture_source(&source, &capture).unwrap());
        });
        assert_eq!(measured.count_total, 1);
        let image = image.unwrap();
        drop((compiled, owner, source));
        assert_eq!(imported.memory().used(), 0);
        assert_eq!(saved_bytes(&image), bytes);
        assert!(native_memory.used() > 0);
        let restored_control = StorageReadControl::with_limit(128 << 20);
        let restored_owner = AnalyzerResources::new(AnalyzerLimits::default());
        let restored = read_analyzer_image(
            &mut saved_bytes(&image).as_slice(),
            &restored_owner,
            &restored_control,
        )
        .unwrap();
        assert_eq!(restored.analyze_tokens(query).unwrap(), expected);
        drop((restored, restored_owner, image));
        assert_eq!(restored_control.memory().used(), 0);
        assert_eq!(capture.memory().used(), 0);
        assert_eq!(native_memory.used(), 0);
    }
}
