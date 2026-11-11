//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::nori::DictionaryError;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("user rules borrow original source")
    }
}

fn input(
    source: Arc<str>,
    model: Arc<NoriDictionary>,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<Option<Arc<UserDictionary>>, DictionaryError> {
    ImageDecoder::new(memory, token, |mut channel| async move {
        UserDictionary::compile_with(
            &source,
            &model,
            UserDictionaryLimits::default(),
            &mut channel.input(),
        )
        .await
    })
    .unwrap()
}

#[test]
fn retained_korean_user_preparation_preserves_stable_rules_and_retires_partial_owners() {
    let model = super::super::NoriResources::default()
        .load_default()
        .unwrap();
    let prefix = "尾".repeat(256);
    let mut names: Vec<_> = (0..128).rev().map(|i| format!("{prefix}{i:04}")).collect();
    let source: Arc<str> =
        format!("{}\n세종시 세종 시\n세종시 세 종 시\n", names.join("\r\n")).into();
    names.push("세종시".into());
    names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
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
        let mut decoder = input(source.clone(), model.model().clone(), &memory, &token);
        let mut calls = 0;
        while calls < stop {
            if let Some(value) = decoder
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
            {
                assert_eq!(stop, usize::MAX);
                let dictionary = value.unwrap();
                assert_eq!(dictionary.source(), source.as_ref());
                assert_eq!(dictionary.len(), names.len());
                for (rank, name) in names.iter().enumerate() {
                    assert_eq!(dictionary.lookup(name), Some(rank as u32));
                }
                let duplicate = dictionary
                    .entry(dictionary.lookup("세종시").unwrap())
                    .unwrap();
                assert_eq!(duplicate.segment_lengths(), Some([2, 1].as_slice()));
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
            assert!(decoder
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })
                .is_err());
            assert_eq!(Arc::strong_count(&source), 1);
            assert_eq!(memory.used(), 0);
        }
        drop(decoder);
        assert_eq!(Arc::strong_count(&source), 1);
        assert_eq!(memory.used(), 0);
    }
    assert!(total > 1024);
}
