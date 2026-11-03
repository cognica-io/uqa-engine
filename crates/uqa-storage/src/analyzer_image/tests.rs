//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_analysis::{Analyzer, AnalyzerLimits};

fn encode(source: &CompiledAnalyzer) -> Vec<u8> {
    let mut bytes = Vec::new();
    let written =
        write_analyzer_image(source, &mut bytes, &StorageReadControl::with_limit(0)).unwrap();
    assert_eq!(written, bytes.len() as u64);
    bytes
}

#[test]
fn analyzer_image_matches_independent_outer_fields_and_restores_complete_token_observations() {
    let owner = AnalyzerResources::new(AnalyzerLimits::default());
    let source = owner
        .compile(&uqa_analysis::standard_analyzer("english"))
        .unwrap();
    let descriptor = source.descriptor().canonical_json().as_bytes();
    let mut expected = b"UQAANA01".to_vec();
    expected.extend((descriptor.len() as u64).to_le_bytes());
    expected.extend(descriptor);
    expected.extend(0_u64.to_le_bytes());
    assert_eq!(encode(&source), expected);
    let observations = source.analyze_tokens("The cats and dogs").unwrap();
    drop(source);
    let control = StorageReadControl::with_limit(1 << 20);
    let restored = read_analyzer_image(&mut expected.as_slice(), &owner, &control).unwrap();
    assert_eq!(
        restored.analyze_tokens("The cats and dogs").unwrap(),
        observations
    );
    assert_eq!(encode(&restored), expected);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn analyzer_image_rejects_every_prefix_trailing_data_and_unfunded_input() {
    let owner = AnalyzerResources::new(AnalyzerLimits::default());
    let source = owner.compile(&Analyzer::default()).unwrap();
    let bytes = encode(&source);
    let control = StorageReadControl::with_limit(1 << 20);
    for end in 0..bytes.len() {
        assert!(
            read_analyzer_image(&mut &bytes[..end], &owner, &control).is_err(),
            "prefix {end}"
        );
        assert_eq!(control.memory().used(), 0);
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(read_analyzer_image(&mut trailing.as_slice(), &owner, &control).is_err());
    let mut wrong = bytes.clone();
    wrong[0] ^= 1;
    assert!(read_analyzer_image(&mut wrong.as_slice(), &owner, &control).is_err());
    let small = StorageReadControl::with_limit(0);
    assert!(matches!(
        read_analyzer_image(&mut bytes.as_slice(), &owner, &small),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(small.memory().used(), 0);
    let limited = AnalyzerResources::new(AnalyzerLimits {
        max_descriptor_bytes: 1,
        ..Default::default()
    });
    assert!(read_analyzer_image(&mut bytes.as_slice(), &limited, &control).is_err());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn analyzer_image_rejects_invalid_resource_headers_before_payload_allocation() {
    let owner = AnalyzerResources::new(AnalyzerLimits::default());
    let source = owner.compile(&Analyzer::default()).unwrap();
    let plain = encode(&source);
    let count_offset = plain.len() - 8;
    let control = StorageReadControl::with_limit(1 << 20);
    let mut excessive = plain[..count_offset].to_vec();
    excessive.extend(u64::MAX.to_le_bytes());
    assert!(read_analyzer_image(&mut excessive.as_slice(), &owner, &control).is_err());
    assert_eq!(control.memory().used(), 0);
    let mut unknown = plain[..count_offset].to_vec();
    unknown.extend(1_u64.to_le_bytes());
    unknown.push(2);
    assert!(read_analyzer_image(&mut unknown.as_slice(), &owner, &control).is_err());
    assert_eq!(control.memory().used(), 0);
    for (kind, tag) in [
        (AnalyzerResourceKind::Nori, 0),
        (AnalyzerResourceKind::Kuromoji, 1),
    ] {
        let Ok(limit) = owner.image_resource_byte_limit(kind) else {
            continue;
        };
        let mut oversized = plain[..count_offset].to_vec();
        oversized.extend(1_u64.to_le_bytes());
        oversized.push(tag);
        oversized.extend([0; 32]);
        oversized.extend((limit as u64 + 1).to_le_bytes());
        assert!(read_analyzer_image(&mut oversized.as_slice(), &owner, &control).is_err());
        assert_eq!(control.memory().used(), 0);
        for hashes in [[0, 0], [1, 0]] {
            let mut unordered = plain[..count_offset].to_vec();
            unordered.extend(2_u64.to_le_bytes());
            for hash in hashes {
                unordered.push(tag);
                unordered.extend([hash; 32]);
                unordered.extend(0_u64.to_le_bytes());
            }
            assert!(
                matches!(read_analyzer_image(&mut unordered.as_slice(), &owner, &control), Err(StorageBackendError::Other(message)) if message == "invalid analyzer image")
            );
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn analyzer_image_output_polls_between_short_writes_and_retains_cancellation_kind() {
    struct Writer<'a> {
        control: &'a StorageReadControl,
        calls: usize,
        bytes: usize,
    }
    impl Write for Writer<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            assert!(bytes.len() <= 4096);
            let written = bytes.len().min(3);
            self.calls += 1;
            self.bytes += written;
            if self.calls == 5 {
                self.control.cancellation().cancel();
            }
            Ok(written)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let owner = AnalyzerResources::default();
    let source = owner.compile(&Analyzer::default()).unwrap();
    let control = StorageReadControl::with_limit(0);
    let mut writer = Writer {
        control: &control,
        calls: 0,
        bytes: 0,
    };
    assert!(matches!(
        write_analyzer_image(&source, &mut writer, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(writer.calls, 5);
    assert!(writer.bytes <= 15);
    assert!(matches!(
        read_analyzer_image(&mut [].as_slice(), &owner, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
}

#[test]
fn analyzer_dictionary_images_preserve_enabled_languages_and_release_import_admission() {
    let owner = AnalyzerResources::new(AnalyzerLimits::default());
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
        if owner.image_resource_byte_limit(kind).is_err() {
            let plain = owner.compile(&Analyzer::default()).unwrap();
            let mut unavailable = encode(&plain);
            let count = unavailable.len() - 8;
            unavailable[count..].copy_from_slice(&1_u64.to_le_bytes());
            unavailable.push(match kind {
                AnalyzerResourceKind::Nori => 0,
                AnalyzerResourceKind::Kuromoji => 1,
            });
            let control = StorageReadControl::with_limit(1 << 20);
            assert!(matches!(
                read_analyzer_image(&mut unavailable.as_slice(), &owner, &control),
                Err(StorageBackendError::Analysis(
                    uqa_analysis::AnalysisError::Descriptor(_)
                ))
            ));
            assert_eq!(control.memory().used(), 0);
            continue;
        }
        let config: Analyzer = serde_json::from_value(serde_json::json!({
            "tokenizer": { "type": tokenizer }, "normalization": {"type": "unavailable"}
        }))
        .unwrap();
        let source = owner.compile(&config).unwrap();
        assert_eq!(source.dictionary_resources().count(), 1);
        let expected = source.analyze_tokens(query).unwrap();
        let bytes = encode(&source);
        let descriptor_end = 16 + source.descriptor().canonical_json().len();
        drop(source);
        let control = StorageReadControl::with_limit(128 << 20);
        let restored = read_analyzer_image(&mut bytes.as_slice(), &owner, &control).unwrap();
        assert_eq!(restored.analyze_tokens(query).unwrap(), expected);
        assert_eq!(encode(&restored), bytes);
        assert!(control.memory().used() > 0);
        let nested = restored.clone();
        drop(restored);
        assert!(control.memory().used() > 0);
        drop(nested);
        assert_eq!(control.memory().used(), 0);
        let mut missing = bytes[..descriptor_end].to_vec();
        missing.extend(0_u64.to_le_bytes());
        assert!(read_analyzer_image(&mut missing.as_slice(), &owner, &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
}
