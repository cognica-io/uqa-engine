//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, MemoryBudget},
    CancellationToken,
};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("typed scalar cannot reread transport bytes")
    }
}

fn decoder(
    text: &str,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<u32, JsonInputError> {
    decoder_with_depth(text, memory, token, 128)
}

fn decoder_with_depth(
    text: &str,
    memory: &MemoryBudget,
    token: &CancellationToken,
    depth: usize,
) -> ImageDecoder<u32, JsonInputError> {
    let text = Budgeted::new(text.to_owned(), memory.reserve(text.len()).unwrap());
    ImageDecoder::new(memory, token, |mut channel| async move {
        let mut work = channel.input();
        let mut reader =
            JsonReader::new(&text, work.memory, work.cancellation).with_depth_limit(depth);
        let value = reader.read_u32_from(&mut work).await?;
        assert!(reader
            .next_event_from(&mut work)
            .await
            .map_err(|error| JsonInputError::from_reader(&reader, error))?
            .is_none());
        Ok(value)
    })
    .unwrap()
}

#[test]
fn typed_json_u32_type_errors_precede_container_admission_and_redact_scalars() {
    for text in ["[", "{", "-42", "0.5", "-0"] {
        let memory = MemoryBudget::new(65536);
        let mut read = decoder_with_depth(text, &memory, &CancellationToken::new(), 0);
        let occupied = text
            .starts_with(['[', '{'])
            .then(|| memory.reserve(memory.available()).unwrap());
        let JsonInputError::Diagnostic(error) = finish(&mut read, 1).unwrap_err() else {
            panic!("typed value must reject before container admission")
        };
        assert_eq!(
            error.to_string(),
            serde_json::from_str::<u32>(text).unwrap_err().to_string()
        );
        let redacted = error.into_type_only().to_string();
        if text == "-42" {
            assert!(redacted.starts_with("invalid value: integer, expected u32"));
        }
        if text == "0.5" || text == "-0" {
            assert!(redacted.starts_with("invalid type: floating point, expected u32"));
        }
        drop((read, occupied));
        assert_eq!(memory.used(), 0);
    }
}

fn finish(
    read: &mut ImageDecoder<u32, JsonInputError>,
    steps: usize,
) -> Result<u32, JsonInputError> {
    for _ in 0..100_000 {
        if let Some(value) = read.advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })? {
            return Ok(value);
        }
    }
    panic!("typed scalar input stopped progressing")
}

fn compare(text: &str) {
    let expected = serde_json::from_str::<u32>(text);
    for steps in [1, 13] {
        let memory = MemoryBudget::new(1 << 20);
        let mut read = decoder(text, &memory, &CancellationToken::new());
        let actual = finish(&mut read, steps);
        match (&expected, actual) {
            (Ok(expected), Ok(actual)) => assert_eq!(*expected, actual),
            (Err(expected), Err(JsonInputError::Diagnostic(actual))) => {
                assert_eq!(actual.to_string(), expected.to_string(), "{text:?}");
                assert_eq!(actual.position().line, expected.line());
                assert_eq!(actual.position().column, expected.column());
                assert_eq!(actual.is_data(), expected.is_data());
                assert_eq!(actual.is_syntax(), expected.is_syntax());
                assert_eq!(actual.is_eof(), expected.is_eof());
                let position = actual.position();
                let before = &text.as_bytes()[..position.byte_offset];
                assert_eq!(position.line, before.split(|&b| b == b'\n').count());
                assert_eq!(
                    position.column,
                    before
                        .iter()
                        .rposition(|&b| b == b'\n')
                        .map_or(before.len(), |i| before.len() - i - 1)
                );
                let redacted = actual.into_type_only();
                assert_eq!(redacted.position(), position);
                assert_eq!(redacted.is_data(), expected.is_data());
            }
            (expected, actual) => panic!("{text:?}: {expected:?} != {actual:?}"),
        }
        drop(read);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn typed_json_u32_preserves_original_values_error_categories_and_scalar_positions() {
    for value in [
        "0",
        "-0",
        "1",
        "4294967295",
        "4294967296",
        "-1",
        "-9223372036854775808",
        "-9223372036854775809",
        "18446744073709551615",
        "18446744073709551616",
        "1.0",
        "1.5",
        "1e0",
        "0e0",
        "1e-400",
        "1e400",
        "1e999999999999999999999",
        "0e999999999999999999999",
        "1e-999999999999999999999",
        "-",
        "-\n",
        "-x",
        "-🙂",
        "1.한",
        "1e한",
        "1.",
        "1.\n",
        "1e",
        "1e+",
        "1e+\n",
        "01",
        "true",
        "null",
        "\"한🙂\"",
        "[",
        "{",
        "+1",
    ] {
        for source in [
            value.to_owned(),
            format!(" \n {value}\t"),
            format!(" \n {value} trailing"),
        ] {
            for end in (0..=source.len()).filter(|&end| source.is_char_boundary(end)) {
                compare(&source[..end]);
            }
        }
    }
    for source in ["4294967295", "-9223372036854775809", "1e+400"] {
        for offset in 0..source.len() {
            for byte in [0, b'-', b'.', b'e', b'\n', b' ', b']'] {
                let mut changed = source.as_bytes().to_vec();
                changed[offset] = byte;
                compare(std::str::from_utf8(&changed).unwrap());
            }
        }
    }
}

#[test]
fn typed_json_numeric_probe_fits_its_fixed_scratch_allowance() {
    for input in [
        "0".into(),
        "-0".into(),
        "4294967296".into(),
        "-9223372036854775809".into(),
        "1e400".into(),
        "1e+\n".into(),
        format!("1e{} trailing", "9".repeat(65536)),
        format!("0e{} trailing", "9".repeat(65536)),
        format!("0.{}1 trailing", "0".repeat(65536)),
        "9".repeat(65536),
    ] {
        let measured = allocation_counter::measure(|| {
            assert!(observe(input.as_bytes()).is_ok());
        });
        assert!(measured.bytes_max <= SCALAR_SCRATCH as u64, "{measured:?}");
        assert_eq!(measured.bytes_current, 0);
    }
}

#[test]
fn typed_json_numeric_work_retires_cancelled_denied_and_abandoned_input() {
    let source = format!("0.{}1", "0".repeat(65536));
    let memory = MemoryBudget::new(2 << 20);
    let token = CancellationToken::new();
    let window = ImageDecodeWindow { bytes: 1, steps: 1 };
    let mut read = decoder(&source, &memory, &token);
    let mut polls = 0;
    loop {
        match read.advance(&mut NoRead, window) {
            Ok(None) => polls += 1,
            Err(JsonInputError::Diagnostic(error)) => {
                assert!(error.is_data());
                break;
            }
            actual => panic!("unexpected numeric result: {actual:?}"),
        }
    }
    assert!(polls >= source.len() / 4096);
    drop(read);
    assert_eq!(memory.used(), 0);
    for boundary in 1..=polls {
        for cancel in [false, true] {
            let token = CancellationToken::new();
            let mut read = decoder(&source, &memory, &token);
            for _ in 0..boundary {
                assert!(read.advance(&mut NoRead, window).unwrap().is_none());
            }
            if cancel {
                token.cancel();
                assert!(matches!(
                    read.advance(&mut NoRead, window),
                    Err(JsonInputError::Cancelled(_))
                ));
                assert!(read.advance(&mut NoRead, window).is_err());
            }
            drop(read);
            assert_eq!(memory.used(), 0);
        }
    }
    let mut read = decoder("1", &memory, &CancellationToken::new());
    let occupied = memory.reserve(memory.available()).unwrap();
    assert!(matches!(
        finish(&mut read, 1),
        Err(JsonInputError::Memory(_))
    ));
    assert!(read.advance(&mut NoRead, window).is_err());
    drop((read, occupied));
    assert_eq!(memory.used(), 0);
}
