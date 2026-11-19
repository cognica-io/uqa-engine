//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{decode_with, Reader};
use crate::morphology::{work::inline, DictionaryError};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder, ImageIoError},
    memory::MemoryBudget,
    CancellationToken,
};

const WINDOW: ImageDecodeWindow = ImageDecodeWindow { bytes: 1, steps: 1 };

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("native string input must only visit its captured section")
    }
}

fn wire(strings: &[&[u8]]) -> Arc<Vec<u8>> {
    let mut bytes = (strings.len() as u32).to_le_bytes().to_vec();
    for text in strings {
        bytes.extend_from_slice(&(text.len() as u32).to_le_bytes());
        bytes.extend_from_slice(text);
    }
    Arc::new(bytes)
}

fn input(
    bytes: Arc<Vec<u8>>,
    maximum_strings: usize,
    maximum_text: usize,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<Vec<String>, DictionaryError> {
    ImageDecoder::new(memory, token, |mut channel| async move {
        let mut reader = Reader::new(&bytes, "string pool", false);
        let value = decode_with(
            &mut reader,
            maximum_strings,
            maximum_text,
            &mut channel.input(),
        )
        .await?;
        reader.finish()?;
        Ok(value)
    })
    .unwrap()
}

#[test]
fn dictionary_string_input_preserves_split_unicode_limits_and_native_capacities() {
    let text = format!("{}{}", "x".repeat(4095), "가\u{10400}ä".repeat(20_000));
    let mut expected = vec![""; 4097];
    expected.extend([text.as_str(), "\0𠮷한A"]);
    let bytes = wire(
        &expected
            .iter()
            .map(|text| text.as_bytes())
            .collect::<Vec<_>>(),
    );
    let units = 4095 + 4 * 20_000;
    for steps in [1, 3, 17, 512] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = input(bytes.clone(), expected.len(), units, &memory, &token);
        let mut calls = 0;
        let strings = loop {
            calls += 1;
            assert!(calls < 10_000);
            if let Some(strings) = input
                .advance(
                    &mut NoRead,
                    ImageDecodeWindow {
                        bytes: calls % 23 + 1,
                        steps,
                    },
                )
                .unwrap()
            {
                break strings;
            }
        };
        if steps == 1 {
            assert!(calls > 4097 / 128 + 2 * text.len() / 4096);
        }
        assert_eq!(strings, expected);
        assert_eq!(strings.capacity(), expected.len());
        for (actual, expected) in strings.iter().zip(&expected) {
            assert_eq!(actual.capacity(), expected.len());
        }
        assert_eq!(memory.used(), 0);
        assert!(input.advance(&mut NoRead, WINDOW).is_err());
    }
}

#[test]
fn dictionary_string_input_preserves_global_utf8_diagnostics_before_length_limits() {
    let tails: &[&[u8]] = &[
        &[0xff],
        &[0xc2],
        &[0xe1, 0x80],
        &[0xf0, 0x90, 0x80],
        &[0xf0, 0x80, 0x80, 0x80],
        &[0xed, 0xa0, 0x80],
        &[0xf4, 0x90, 0x80, 0x80],
        &[0xe2, 0x82, b'x'],
        &[0xf0, 0x90, 0x80, b'x'],
    ];
    for prefix in [0, 1, 4093, 4094, 4095, 4096, 8191, 160_000] {
        for tail in tails {
            let mut invalid = vec![b'x'; prefix];
            invalid.extend_from_slice(tail);
            let expected = std::str::from_utf8(&invalid).unwrap_err();
            let bytes = wire(&[b"", &invalid]);
            let ordinary = inline(decode_with(
                &mut Reader::new(&bytes, "string pool", false),
                2,
                1,
                &mut (),
            ))
            .unwrap_err();
            let DictionaryError::Utf8(ordinary) = ordinary else {
                panic!("ordinary input changed its native UTF-8 error variant");
            };
            assert_eq!(ordinary, expected);
            let measured = allocation_counter::measure(|| {
                let memory = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let mut input = input(bytes.clone(), 2, 1, &memory, &token);
                let mut calls = 0;
                let error = loop {
                    calls += 1;
                    match input.advance(&mut NoRead, WINDOW) {
                        Ok(None) => (),
                        Ok(Some(_)) => panic!("invalid UTF-8 was accepted"),
                        Err(error) => break error,
                    }
                };
                assert!(calls > prefix / 4096);
                let DictionaryError::Utf8Offset {
                    offset,
                    error: native,
                } = &error
                else {
                    panic!("retained input lost its native fragment position");
                };
                assert_eq!(*offset + native.valid_up_to(), expected.valid_up_to());
                assert_eq!(native.error_len(), expected.error_len());
                assert_eq!(
                    error.to_string(),
                    format!("invalid UTF-8 in dictionary: {expected}")
                );
                #[cfg(feature = "nori")]
                assert_eq!(
                    crate::nori::DictionaryError::from(DictionaryError::Utf8Offset {
                        offset: *offset,
                        error: *native
                    })
                    .to_string(),
                    format!("invalid UTF-8 in Nori dictionary: {expected}"),
                );
                #[cfg(feature = "kuromoji")]
                assert_eq!(
                    crate::kuromoji::DictionaryError::from(DictionaryError::Utf8Offset {
                        offset: *offset,
                        error: *native
                    })
                    .to_string(),
                    format!("invalid UTF-8 in Kuromoji dictionary: {expected}"),
                );
                assert!(input.advance(&mut NoRead, WINDOW).is_err());
                drop(input);
                assert_eq!(memory.used(), 0);
            });
            assert_eq!(measured.bytes_current, 0, "prefix {prefix}, tail {tail:?}");
        }
    }
}

#[test]
fn dictionary_string_input_retires_completed_and_partial_strings_at_every_copy_poll() {
    let first = "\u{1f600}".repeat(16_384);
    let second = "中".repeat(20_000);
    let bytes = wire(&[first.as_bytes(), second.as_bytes()]);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut reference = input(bytes.clone(), 2, 32_768, &memory, &token);
    let mut calls = 0;
    loop {
        calls += 1;
        if reference.advance(&mut NoRead, WINDOW).unwrap().is_some() {
            break;
        }
    }
    assert!(calls > 60);
    for stop in 0..calls {
        for mode in ["drop", "cancel", "window"] {
            let measured = allocation_counter::measure(|| {
                let memory = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let mut input = input(bytes.clone(), 2, 32_768, &memory, &token);
                for _ in 0..stop {
                    assert!(input.advance(&mut NoRead, WINDOW).unwrap().is_none());
                }
                match mode {
                    "cancel" => {
                        token.cancel();
                        assert!(matches!(
                            input.advance(&mut NoRead, WINDOW),
                            Err(DictionaryError::Input(ImageIoError::Cancelled(_)))
                        ));
                    }
                    "window" => {
                        assert!(matches!(
                            input.advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 0 }),
                            Err(DictionaryError::Input(ImageIoError::Invalid))
                        ));
                    }
                    _ => (),
                }
                if mode != "drop" {
                    assert!(input.advance(&mut NoRead, WINDOW).is_err());
                }
                drop(input);
                assert_eq!(memory.used(), 0);
            });
            assert_eq!(measured.bytes_current, 0, "stop {stop}, {mode}");
        }
    }
    for (maximum_strings, maximum_text, resource, required) in [
        (1, usize::MAX, "dictionary strings", 2),
        (2, 32_767, "UTF-16 units per dictionary string", 32_768),
    ] {
        let mut input = input(
            bytes.clone(),
            maximum_strings,
            maximum_text,
            &memory,
            &token,
        );
        let error = loop {
            match input.advance(&mut NoRead, WINDOW) {
                Ok(None) => (),
                Ok(Some(_)) => panic!("exceeded native limit was accepted"),
                Err(error) => break error,
            }
        };
        assert!(
            matches!(error, DictionaryError::Limit { resource: actual_resource, required: actual_required, .. } if actual_resource == resource && actual_required == required)
        );
        assert_eq!(memory.used(), 0);
    }
}
