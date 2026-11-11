//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{CharFilter, TokenFilter, Tokenizer};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    CancellationToken,
};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("compiled preparation borrows the validated descriptor");
    }
}

fn fixture() -> Analyzer {
    Analyzer::new(
        Tokenizer::Keyword,
        vec![
            TokenFilter::Synonym {
                synonyms: (0..256)
                    .map(|i| {
                        (
                            format!("{i:04}{}", "한🙂".repeat(128)),
                            vec!["value".repeat(819)],
                        )
                    })
                    .chain([("alpha".into(), vec!["first".into(), "second".into()])])
                    .collect(),
                synonyms_path: None,
            },
            TokenFilter::Stop {
                language: String::new(),
                custom_words: (0..256)
                    .map(|i| format!("stop{i:04}{}", "한🙂".repeat(256)))
                    .chain(["second".into()])
                    .collect(),
            },
        ],
        vec![CharFilter::Mapping {
            mapping: (0..256)
                .map(|i| (format!("m{i:04}{}", "尾".repeat(256)), "target".repeat(512)))
                .chain([("source".into(), "alpha".into())])
                .collect(),
        }],
    )
}

fn preparing(
    descriptor: Arc<AnalyzerDescriptor>,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<CompiledAnalyzer, AnalysisError> {
    ImageDecoder::new(memory, token, |mut channel| async move {
        CompiledAnalyzer::prepare_with(
            descriptor,
            #[cfg(feature = "nori")]
            crate::nori::pipeline::ResolvedNoriPipeline::default(),
            #[cfg(feature = "kuromoji")]
            crate::kuromoji::pipeline::ResolvedKuromojiPipeline::default(),
            &mut channel.input(),
        )
        .await
    })
    .unwrap()
}

#[test]
fn compiled_preparation_retains_configuration_and_stage_work_without_whole_pipeline_copies() {
    let config = fixture();
    let resources = AnalyzerResources::new(crate::AnalyzerLimits::default());
    let ordinary = resources.compile(&config).unwrap();
    let expected = ordinary.analyze("source").unwrap();
    assert_eq!(expected, ["alpha", "first"]);
    let expected_config = serde_json::to_value(ordinary.configuration()).unwrap();
    let descriptor = Arc::clone(ordinary.descriptor());
    drop((ordinary, resources));
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = preparing(Arc::clone(&descriptor), &memory, &token);
    let mut calls = 0;
    let restored = loop {
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap();
        });
        assert!(
            allocation.bytes_total < 64 * 1024,
            "compiled preparation poll {calls} allocated {} bytes",
            allocation.bytes_total
        );
        calls += 1;
        if let Some(result) = result {
            break result;
        }
        assert!(calls < 100_000);
    };
    assert!(calls > 1024);
    assert_eq!(restored.analyze("source").unwrap(), expected);
    assert!(serde_json::to_value(restored.configuration()).unwrap() == expected_config);
    assert!(Arc::ptr_eq(restored.descriptor(), &descriptor));
    drop((input, restored));
    assert_eq!(Arc::strong_count(&descriptor), 1);
    assert_eq!(memory.used(), 0);
}

#[test]
fn compiled_preparation_retires_configuration_and_prepared_stages_at_each_boundary() {
    let resources = AnalyzerResources::new(crate::AnalyzerLimits::default());
    let compiled = resources.compile(&fixture()).unwrap();
    let descriptor = Arc::clone(compiled.descriptor());
    drop((compiled, resources));
    let mut total = 0;
    for (fraction, action) in std::iter::once((usize::MAX, 0))
        .chain((0..5).flat_map(|fraction| (0..3).map(move |action| (fraction, action))))
    {
        let stop = if fraction == usize::MAX {
            usize::MAX
        } else {
            total * fraction / 5
        };
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = preparing(Arc::clone(&descriptor), &memory, &token);
        let mut calls = 0;
        while calls < stop {
            if input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
                .is_some()
            {
                assert_eq!(stop, usize::MAX);
                total = calls;
                break;
            }
            calls += 1;
            assert!(calls < 100_000);
        }
        if stop != usize::MAX && action != 0 {
            if action == 1 {
                token.cancel();
            }
            let steps = usize::from(action == 1);
            assert!(input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })
                .is_err());
            assert_eq!(Arc::strong_count(&descriptor), 1);
            assert_eq!(memory.used(), 0);
            assert!(input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .is_err());
        }
        drop(input);
        assert_eq!(Arc::strong_count(&descriptor), 1);
        assert_eq!(memory.used(), 0);
    }
    assert!(total > 1024);
}

#[test]
fn retained_replacement_preparation_matches_regex_capture_expansion() {
    let pattern = "(?P<word>alpha)(?P<suffix>[0-9]+)?";
    let text = "before alpha42 / alpha / after";
    let expression = regex::Regex::new(pattern).unwrap();
    let replacements = [
        "${word}:$suffix:$1:$0:$$:${missing}:$99:${}:$word_tail:$".repeat(256),
        format!(
            "{}${{word}}{}${{unterminated",
            "한🙂".repeat(4096),
            "尾".repeat(4096)
        ),
        format!("${{{}}}:$word", "unknown".repeat(4096)),
        "plain한🙂".repeat(4096),
    ];
    for replacement in replacements {
        let expected = expression
            .replace_all(text, replacement.as_str())
            .into_owned();
        let config = Arc::new(CharFilter::PatternReplace {
            pattern: pattern.into(),
            replacement,
        });
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let source = Arc::clone(&config);
        let mut input = ImageDecoder::new(&memory, &token, |mut channel| async move {
            let mut work = channel.input();
            source
                .prepare_with(&mut work)
                .await?
                .into_owned_with(&mut work)
                .await
        })
        .unwrap();
        let mut calls = 0;
        let restored = loop {
            calls += 1;
            if let Some(value) = input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
            {
                break value;
            }
            assert!(calls < 100_000);
        };
        assert!(calls > 10);
        assert_eq!(Arc::strong_count(&config), 1);
        assert_eq!(memory.used(), 0);
        drop(config);
        assert_eq!(
            restored
                .filter_mapped(FilteredText::new(text))
                .unwrap()
                .as_str(),
            expected
        );
    }
}
