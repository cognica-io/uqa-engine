//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::ProductionControl;

fn capture(
    source: &CompiledAnalyzer,
    control: &StorageReadControl,
    native: bool,
) -> AnalyzerImageSnapshot {
    if native {
        let image = source
            .image_source_with_control(&ProductionControl::uncontrolled())
            .unwrap();
        AnalyzerImageSnapshot::capture_source(&image, control).unwrap()
    } else {
        AnalyzerImageSnapshot::capture(source, control).unwrap()
    }
}

fn drain(reader: &mut ImageReader<'_>, window: usize) -> Vec<u8> {
    assert_eq!(reader.read(&mut []).unwrap(), 0);
    let progress = reader.prepare(1).unwrap();
    assert!(progress.ready);
    assert_eq!(progress.positions, 0);
    let mut output = Vec::new();
    let mut buffer = vec![0; window];
    loop {
        buffer.fill(0xa5);
        let count = reader.read(&mut buffer).unwrap();
        assert!(count <= window.min(16 * 1024));
        assert!(buffer[count..].iter().all(|&byte| byte == 0xa5));
        if count == 0 {
            return output;
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

#[test]
fn resumable_analyzer_images_match_literal_framing_after_source_loss_and_thread_transfer() {
    for native in [false, true] {
        let owner = AnalyzerResources::new(AnalyzerLimits::default());
        let source = owner
            .compile(&uqa_analysis::standard_analyzer("english"))
            .unwrap();
        let weak = Arc::downgrade(&source);
        let descriptor = source.descriptor().canonical_json().as_bytes();
        let mut expected = b"UQAANA01".to_vec();
        expected.extend((descriptor.len() as u64).to_le_bytes());
        expected.extend(descriptor);
        expected.extend(0_u64.to_le_bytes());
        let observations = source.analyze_tokens("The cats and dogs").unwrap();
        let admission = StorageReadControl::with_limit(1 << 20);
        let image = capture(&source, &admission, native);
        drop((source, owner));
        assert!(weak.upgrade().is_none());
        for window in [1, 7, 4095, 32 * 1024] {
            let control = StorageReadControl::with_limit(64 * 1024);
            let mut reader = image.image_reader(&control).unwrap();
            assert_eq!(drain(&mut reader, window), expected);
            assert_eq!(control.memory().used(), 0);
        }
        let control = StorageReadControl::with_limit(64 * 1024);
        let mut reader = image.into_image_reader(&control).unwrap();
        let bytes = std::thread::spawn(move || drain(&mut reader, 3))
            .join()
            .unwrap();
        assert_eq!(bytes, expected);
        assert_eq!(admission.memory().used(), 0);
        assert_eq!(control.memory().used(), 0);
        let restored = read_analyzer_image(
            &mut bytes.as_slice(),
            &AnalyzerResources::default(),
            &control,
        )
        .unwrap();
        assert_eq!(
            restored.analyze_tokens("The cats and dogs").unwrap(),
            observations
        );
    }
}

#[test]
fn resumable_analyzer_output_bounds_large_descriptors_and_exact_cursor_admission() {
    let source = Analyzer::new(
        uqa_analysis::Tokenizer::Whitespace,
        vec![uqa_analysis::TokenFilter::Stop {
            language: "english".into(),
            custom_words: (0..4096)
                .map(|i| format!("{i}{}", "x".repeat(120)))
                .collect(),
        }],
        Vec::new(),
    )
    .compile()
    .unwrap();
    let expected = encode(&source);
    assert!(expected.len() > 400 * 1024);
    for native in [false, true] {
        let source_control = StorageReadControl::with_limit(2 << 20);
        let source = capture(&source, &source_control, native);
        let probe = StorageReadControl::with_limit(64 * 1024);
        let cursor = source.image_reader(&probe).unwrap();
        let needed = probe.memory().used();
        drop(cursor);
        assert_eq!(probe.memory().used(), 0);
        let denied = StorageReadControl::with_limit(needed - 1);
        assert!(matches!(
            source.image_reader(&denied),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(denied.memory().used(), 0);
        let exact = StorageReadControl::with_limit(needed);
        let mut reader = source.image_reader(&exact).unwrap();
        let mut buffer = vec![0; 32 * 1024];
        let mut offset = 0;
        let allocation = allocation_counter::measure(|| loop {
            let count = reader.read(&mut buffer).unwrap();
            assert!(count <= 16 * 1024);
            if count == 0 {
                break;
            }
            assert_eq!(&buffer[..count], &expected[offset..offset + count]);
            offset += count;
        });
        assert_eq!(allocation.count_total, 0);
        assert_eq!(offset, expected.len());
        assert_eq!(exact.memory().used(), 0);
    }
}

#[test]
fn owned_analyzer_readers_retire_both_cancellation_scopes_and_denied_admission() {
    for native in [false, true] {
        for original in [false, true] {
            for empty in [false, true] {
                let source = Analyzer::default().compile().unwrap();
                let admission = StorageReadControl::with_limit(1 << 20);
                let image = capture(&source, &admission, native);
                let control = StorageReadControl::with_limit(64 * 1024);
                let mut reader = image.into_image_reader(&control).unwrap();
                assert_eq!(reader.read(&mut [0; 7]).unwrap(), 7);
                if original {
                    admission.cancellation().cancel();
                } else {
                    control.cancellation().cancel();
                }
                let error = if empty {
                    reader.read(&mut [])
                } else {
                    reader.read(&mut [0; 7])
                };
                assert!(matches!(error, Err(StorageBackendError::Cancelled(_))));
                assert!(reader.prepare(1).is_err());
                assert!(reader.read(&mut [0; 7]).is_err());
                assert_eq!(admission.memory().used(), 0);
                assert_eq!(control.memory().used(), 0);
                assert_eq!(source.analyze("still usable").unwrap(), ["still", "usable"]);
            }
        }
        let source = Analyzer::default().compile().unwrap();
        let admission = StorageReadControl::with_limit(1 << 20);
        let image = capture(&source, &admission, native);
        let denied = StorageReadControl::with_limit(0);
        assert!(matches!(
            image.into_image_reader(&denied),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(admission.memory().used(), 0);
        assert_eq!(denied.memory().used(), 0);
    }
}

#[test]
fn resumable_analyzer_dictionary_images_keep_exact_enabled_artifacts_and_observations() {
    for (kind, tokenizer, text) in [
        (None, "whitespace", "stable tokens"),
        (
            Some(AnalyzerResourceKind::Nori),
            "nori_tokenizer",
            "서울에서 학교에 갔다",
        ),
        (
            Some(AnalyzerResourceKind::Kuromoji),
            "kuromoji_tokenizer",
            "東京都で食べた",
        ),
    ] {
        let owner = AnalyzerResources::new(AnalyzerLimits::default());
        if kind.is_some_and(|kind| owner.image_resource_byte_limit(kind).is_err()) {
            continue;
        }
        let configuration: Analyzer = serde_json::from_value(serde_json::json!({
            "tokenizer": {"type": tokenizer}, "normalization": {"type": "unavailable"}
        }))
        .unwrap();
        let source = owner.compile(&configuration).unwrap();
        let expected = encode(&source);
        let observations = source.analyze_tokens(text).unwrap();
        let admission = StorageReadControl::with_limit(512 << 20);
        let image = capture(&source, &admission, true);
        drop((source, owner));
        let control = StorageReadControl::with_limit(64 * 1024);
        let mut reader = image.into_image_reader(&control).unwrap();
        let bytes = drain(&mut reader, 16 * 1024);
        assert_eq!(bytes, expected);
        assert_eq!(admission.memory().used(), 0);
        assert_eq!(control.memory().used(), 0);
        let restoration = StorageReadControl::with_limit(512 << 20);
        let restored = read_analyzer_image(
            &mut bytes.as_slice(),
            &AnalyzerResources::default(),
            &restoration,
        )
        .unwrap();
        assert_eq!(restored.analyze_tokens(text).unwrap(), observations);
        drop(restored);
        assert_eq!(restoration.memory().used(), 0);
    }
}
