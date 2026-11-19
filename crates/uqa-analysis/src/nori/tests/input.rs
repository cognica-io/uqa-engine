//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::fixtures;
use crate::morphology::io::Writer;
use crate::nori::dictionary::provenance::canonical;
use crate::nori::{frame, DictionaryError, DictionaryLimits, NoriDictionary};
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
        panic!("native model construction must not reread its captured frame")
    }
}

fn sections() -> Vec<frame::Section> {
    let mut sections = fixtures::sections();
    let forward = 193_usize;
    let backward = 127_usize;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(forward as u32).to_le_bytes());
    bytes.extend_from_slice(&(backward as u32).to_le_bytes());
    for index in 0..forward * backward {
        bytes.extend_from_slice(&(index as i16).to_le_bytes());
    }
    sections[3].bytes = bytes;
    sections[3].records = (forward * backward) as u64;
    let mut json = fixtures::provenance();
    json["model"]["matrix_forward"] = forward.into();
    json["model"]["matrix_backward"] = backward.into();
    sections[6].bytes = canonical(&json).unwrap();
    sections
}

fn input(
    bytes: Arc<Vec<u8>>,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<Arc<NoriDictionary>, DictionaryError> {
    ImageDecoder::new(memory, token, |mut channel| async move {
        NoriDictionary::from_bytes_with(&bytes, DictionaryLimits::default(), &mut channel.input())
            .await
    })
    .unwrap()
}

fn native_sections(model: &NoriDictionary) -> Vec<Vec<u8>> {
    let mut values = Vec::new();
    let mut output = Writer::default();
    crate::morphology::surfaces::encode(&model.lexicon, &model.surfaces, &mut output).unwrap();
    values.push(output.0);
    let mut output = Writer::default();
    crate::nori::morphology::encode_words(&model.words, model.known_words, &mut output).unwrap();
    values.push(output.0);
    let mut output = Writer::default();
    model.morphology.encode(&mut output).unwrap();
    values.push(output.0);
    let mut output = Writer::default();
    model.matrix.encode(&mut output).unwrap();
    values.push(output.0);
    let mut output = Writer::default();
    model.characters.encode(&mut output).unwrap();
    values.push(output.0);
    let mut output = Writer::default();
    model.unicode.encode(&mut output).unwrap();
    values.push(output.0);
    values.push(canonical(&model.provenance.json).unwrap());
    values
}

#[test]
fn nori_dictionary_tables_resume_with_exact_native_images_and_no_source_reads() {
    let sections = sections();
    let bytes = Arc::new(frame::encode(&sections, DictionaryLimits::default()).unwrap());
    let ordinary = NoriDictionary::from_bytes(&bytes, DictionaryLimits::default()).unwrap();
    for steps in [1, 3, 17, 512] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut reader = input(bytes.clone(), &memory, &token);
        let mut calls = 0;
        let model = loop {
            calls += 1;
            assert!(calls < 10_000);
            let window = ImageDecodeWindow {
                bytes: calls % 31 + 1,
                steps,
            };
            if let Some(model) = reader.advance(&mut NoRead, window).unwrap() {
                break model;
            }
        };
        if steps == 1 {
            assert!(calls > 65536 / 128 + (193 * 127) / 128);
        }
        assert_eq!(memory.used(), 0);
        assert!(reader.advance(&mut NoRead, WINDOW).is_err());
        assert_eq!(model.id(), ordinary.id());
        assert_eq!(
            native_sections(&model),
            sections.iter().map(|s| s.bytes.clone()).collect::<Vec<_>>()
        );
        assert_eq!(model.matrix.costs.capacity(), 193 * 127);
        assert_eq!(model.characters.values.capacity(), 65536);
        assert_eq!(
            model.connection_cost(192, 126),
            Some((193 * 127 - 1) as i16)
        );
    }
}

#[test]
fn nori_dictionary_table_failure_retires_the_complete_private_candidate() {
    let bytes = Arc::new(frame::encode(&sections(), DictionaryLimits::default()).unwrap());
    let original = NoriDictionary::from_bytes(&bytes, DictionaryLimits::default()).unwrap();
    let expected = native_sections(&original);
    for stop in [0, 1, 2, 64, 128, 256, 512] {
        for mode in ["cancel", "window", "drop"] {
            let measured = allocation_counter::measure(|| {
                let memory = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let mut reader = input(bytes.clone(), &memory, &token);
                for _ in 0..stop {
                    assert!(reader.advance(&mut NoRead, WINDOW).unwrap().is_none());
                }
                if mode != "drop" {
                    if mode == "cancel" {
                        token.cancel();
                    }
                    let window = ImageDecodeWindow {
                        bytes: 1,
                        steps: usize::from(mode != "window"),
                    };
                    let error = reader.advance(&mut NoRead, window).unwrap_err();
                    match mode {
                        "cancel" => assert!(matches!(
                            error,
                            DictionaryError::Input(ImageIoError::Cancelled(_))
                        )),
                        _ => assert!(matches!(
                            error,
                            DictionaryError::Input(ImageIoError::Invalid)
                        )),
                    }
                    assert!(reader.advance(&mut NoRead, WINDOW).is_err());
                }
                drop(reader);
                assert_eq!(memory.used(), 0);
            });
            assert_eq!(measured.bytes_current, 0, "stop {stop}, {mode}");
            assert_eq!(native_sections(&original), expected);
        }
    }
}

#[test]
fn nori_dictionary_table_rejection_keeps_native_diagnostics_and_releases_all_sections() {
    for mode in ["matrix", "records", "trailing"] {
        let mut sections = sections();
        match mode {
            "matrix" => sections[3].bytes[..4].copy_from_slice(&0_u32.to_le_bytes()),
            "records" => sections[4].records -= 1,
            _ => sections[5].bytes.push(0),
        }
        let bytes = Arc::new(frame::encode(&sections, DictionaryLimits::default()).unwrap());
        let expected = NoriDictionary::from_bytes(&bytes, DictionaryLimits::default())
            .unwrap_err()
            .to_string();
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let measured = allocation_counter::measure(|| {
            let mut reader = input(bytes.clone(), &memory, &token);
            let error = loop {
                match reader.advance(&mut NoRead, WINDOW) {
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("invalid model published: {mode}"),
                    Err(error) => break error,
                }
            };
            assert_eq!(error.to_string(), expected);
            assert!(reader.advance(&mut NoRead, WINDOW).is_err());
            drop(reader);
        });
        assert_eq!(measured.bytes_current, 0, "{mode}");
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn nori_dictionary_string_pools_resume_and_reject_at_absolute_utf8_positions() {
    let text = format!("{}{}", "x".repeat(8191), "가\u{10000}ä".repeat(8000));
    for invalid in [false, true] {
        let mut sections = sections();
        let old = &sections[2].bytes;
        let mut reader = crate::morphology::io::Reader::new(old, "test string pool", false);
        let count = reader.u32().unwrap();
        for _ in 0..count {
            reader.text_bytes().unwrap();
        }
        let end = old.len() - reader.remaining();
        let mut raw = text.as_bytes().to_vec();
        if invalid {
            raw.push(0xff);
        }
        let mut pool = (count + 1).to_le_bytes().to_vec();
        pool.extend_from_slice(&old[4..end]);
        pool.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        pool.extend_from_slice(&raw);
        pool.extend_from_slice(&old[end..]);
        sections[2].bytes = pool;
        let bytes = Arc::new(frame::encode(&sections, DictionaryLimits::default()).unwrap());
        let ordinary = NoriDictionary::from_bytes(&bytes, DictionaryLimits::default());
        if invalid {
            let expected = std::str::from_utf8(&raw).unwrap_err();
            assert!(matches!(ordinary, Err(DictionaryError::Utf8(error)) if error == expected));
            let measured = allocation_counter::measure(|| {
                let memory = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let mut reader = input(bytes.clone(), &memory, &token);
                let error = loop {
                    match reader.advance(&mut NoRead, WINDOW) {
                        Ok(None) => (),
                        Ok(Some(_)) => panic!("invalid pool was published"),
                        Err(error) => break error,
                    }
                };
                let DictionaryError::Utf8Offset {
                    offset,
                    error: native,
                } = error
                else {
                    panic!("retained model input lost the global string position");
                };
                assert_eq!(offset + native.valid_up_to(), expected.valid_up_to());
                assert_eq!(native.error_len(), expected.error_len());
                drop(reader);
                assert_eq!(memory.used(), 0);
            });
            assert_eq!(measured.bytes_current, 0);
        } else {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut reader = input(bytes.clone(), &memory, &token);
            let model = loop {
                if let Some(model) = reader.advance(&mut NoRead, WINDOW).unwrap() {
                    break model;
                }
            };
            assert_eq!(model.id(), ordinary.unwrap().id());
            assert_eq!(model.morphology.strings.last().unwrap(), &text);
            assert_eq!(
                model.morphology.strings.last().unwrap().capacity(),
                text.len()
            );
            assert_eq!(
                native_sections(&model),
                sections.iter().map(|s| s.bytes.clone()).collect::<Vec<_>>()
            );
            assert_eq!(memory.used(), 0);
        }
    }
}
