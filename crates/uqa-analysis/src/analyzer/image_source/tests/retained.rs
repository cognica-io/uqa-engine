//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    image_io::{decode_image, ImageDecodeWindow, ImageDecoder, ImageIoError},
    memory::Budgeted,
};

#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error(transparent)]
    Input(#[from] ImageIoError),
    #[error(transparent)]
    Retention(#[from] ValueRetentionError),
}

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("catalog image preparation cannot reread its transport");
    }
}

fn owned(
    descriptor: Arc<crate::AnalyzerDescriptor>,
    resources: Vec<crate::AnalyzerResourceInput>,
) -> CompiledAnalyzer {
    let owner = AnalyzerResources::new(AnalyzerLimits::default());
    decode_image(
        &mut NoRead,
        &MemoryBudget::new(1 << 20),
        &CancellationToken::new(),
        async |mut input| {
            owner
                .restore_owned_image_with_input(descriptor, resources, &mut input)
                .await
        },
    )
    .unwrap()
}

fn descriptor() -> Arc<crate::AnalyzerDescriptor> {
    let mut words: Vec<_> = (0..256)
        .map(|i| format!("word{i:04}{}", "尾".repeat(256)))
        .collect();
    words.push("skip".into());
    let analyzer = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::Stop {
            language: String::new(),
            custom_words: words,
        }],
        Vec::new(),
    );
    AnalyzerResources::new(AnalyzerLimits::default())
        .compile(&analyzer)
        .unwrap()
        .descriptor()
        .clone()
}

fn preparing(
    compiled: CompiledAnalyzer,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<(Arc<CompiledAnalyzer>, AnalyzerImageSource), Failure> {
    ImageDecoder::new(memory, token, |mut channel| async move {
        Ok(compiled
            .into_image_source_with_input(&mut channel.input())
            .await?)
    })
    .unwrap()
}

#[test]
fn private_analyzer_image_publication_retains_progress_and_preserves_cached_capture_ownership() {
    let descriptor = descriptor();
    let compiled = owned(descriptor.clone(), Vec::new());
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = preparing(compiled, &memory, &token);
    let mut polls = 0;
    let (compiled, source) = loop {
        polls += 1;
        if let Some(value) = input
            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
            .unwrap()
        {
            break value;
        }
        assert!(polls < 10_000);
    };
    assert!(polls > 32);
    assert_eq!(compiled.analyze("keep skip").unwrap(), ["keep"]);
    assert_eq!(source.canonical_json(), descriptor.canonical_json());
    let denied = MemoryBudget::new(0);
    let cached = compiled
        .image_source_with_control(&ProductionControl::new(&denied, &token, &token))
        .unwrap();
    assert!(Arc::ptr_eq(&source.0, &cached.0));
    assert_eq!(denied.used(), 0);
    let weak = Arc::downgrade(&compiled);
    drop((input, compiled, cached));
    assert!(weak.upgrade().is_none());
    assert_eq!(source.canonical_json(), descriptor.canonical_json());
    assert!(memory.used() > 0);
    drop(source);
    assert_eq!(memory.used(), 0);
    assert_eq!(Arc::strong_count(&descriptor), 1);
}

#[test]
fn private_analyzer_image_publication_retires_unshared_revisions_after_terminal_failures() {
    let descriptor = descriptor();
    for stop in [1, 4, 16] {
        for action in 0..3 {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = preparing(owned(descriptor.clone(), Vec::new()), &memory, &token);
            let window = ImageDecodeWindow { bytes: 1, steps: 1 };
            for _ in 0..stop {
                assert!(input.advance(&mut NoRead, window).unwrap().is_none());
            }
            if action == 0 {
                token.cancel();
                assert!(input.advance(&mut NoRead, window).is_err());
            } else if action == 1 {
                assert!(input
                    .advance(&mut NoRead, ImageDecodeWindow { bytes: 0, steps: 1 })
                    .is_err());
            }
            drop(input);
            assert_eq!(memory.used(), 0);
            assert_eq!(Arc::strong_count(&descriptor), 1);
        }
    }
    let memory = MemoryBudget::new(16 << 10);
    let token = CancellationToken::new();
    let input = preparing(owned(descriptor.clone(), Vec::new()), &memory, &token);
    assert!(matches!(
        input.read(&mut NoRead),
        Err(Failure::Retention(ValueRetentionError::Memory(_)))
    ));
    assert_eq!(memory.used(), 0);
    assert_eq!(Arc::strong_count(&descriptor), 1);
}

#[test]
fn private_analyzer_image_publication_detaches_dictionary_bytes_from_import_admission() {
    for (kind, tokenizer, term) in [
        (AnalyzerResourceKind::Nori, "nori_tokenizer", "서울"),
        (AnalyzerResourceKind::Kuromoji, "kuromoji_tokenizer", "東京"),
    ] {
        let owner = AnalyzerResources::new(AnalyzerLimits::default());
        if owner.image_resource_byte_limit(kind).is_err() {
            continue;
        }
        let config: Analyzer = serde_json::from_value(serde_json::json!({
            "tokenizer": {"type": tokenizer}, "normalization": {"type": "unavailable"}
        }))
        .unwrap();
        let compiled = owner.compile(&config).unwrap();
        let imported = MemoryBudget::new(128 << 20);
        let inputs = compiled
            .dictionary_resources()
            .map(|resource| {
                let bytes = resource.bytes().to_vec();
                let admission = imported.reserve(bytes.capacity()).unwrap();
                crate::AnalyzerResourceInput {
                    kind: resource.kind(),
                    sha256: resource.sha256(),
                    bytes: Budgeted::new(bytes, admission).into_shared().unwrap(),
                }
            })
            .collect();
        let descriptor = compiled.descriptor().clone();
        drop((compiled, owner));
        let compiled = owned(descriptor, inputs);
        let input_pointer = compiled
            .dictionary_resources()
            .next()
            .unwrap()
            .bytes()
            .as_ptr();
        let memory = MemoryBudget::new(128 << 20);
        let token = CancellationToken::new();
        let mut input = preparing(compiled, &memory, &token);
        let mut polls = 0;
        let (compiled, source) = loop {
            polls += 1;
            if let Some(value) = input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
            {
                break value;
            }
            assert!(polls < 100_000);
        };
        assert!(polls > 128);
        assert_eq!(compiled.analyze(term).unwrap(), [term]);
        let resource = source.dictionary_resources().next().unwrap();
        let original = compiled.dictionary_resources().next().unwrap();
        assert_eq!(resource.bytes(), original.bytes());
        assert_eq!(resource.sha256(), original.sha256());
        assert_eq!(resource.kind(), kind);
        assert_ne!(resource.bytes().as_ptr(), input_pointer);
        assert!(imported.used() > 0);
        drop((input, compiled));
        assert_eq!(imported.used(), 0);
        assert!(!source
            .dictionary_resources()
            .next()
            .unwrap()
            .bytes()
            .is_empty());
        assert!(memory.used() > 0);
        drop(source);
        assert_eq!(memory.used(), 0);
    }
}
