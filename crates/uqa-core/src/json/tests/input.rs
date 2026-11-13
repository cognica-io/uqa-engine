//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::image_io::{ImageDecodeWindow, ImageDecoder};

fn input(
    text: String,
    expected: &'static str,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<JsonDiagnostic, JsonInputError> {
    ImageDecoder::new(memory, token, |mut reader| async move {
        let mut work = reader.input();
        let mut json = JsonReader::new(&text, work.memory, work.cancellation);
        let event = json
            .next_event_from(&mut work)
            .await
            .map_err(|error| JsonInputError::from_reader(&json, error))?
            .ok_or(JsonInputError::Invalid)?;
        JsonDiagnostic::invalid_type_from(event.token, json.position(), expected, &mut work).await
    })
    .unwrap()
}

fn finish(
    input: &mut ImageDecoder<JsonDiagnostic, JsonInputError>,
) -> Result<JsonDiagnostic, JsonInputError> {
    for _ in 0..100_000 {
        if let Some(value) = input.advance(
            &mut std::io::empty(),
            ImageDecodeWindow { bytes: 1, steps: 1 },
        )? {
            return Ok(value);
        }
    }
    panic!("diagnostic input stopped progressing")
}

#[test]
fn native_json_type_diagnostics_match_direct_deserializer_values_and_positions() {
    for scalar in [
        "null",
        "true",
        "false",
        "0",
        "-0",
        "42",
        "-9223372036854775808",
        "18446744073709551615",
        "18446744073709551616",
        "-9223372036854775809",
        "1.5",
        "1e9999999",
        r#""한\n\u0000\uD83D\uDE00""#,
        "{}",
    ] {
        let text = format!(" \n {scalar} trailing");
        let reference = serde_json::from_str::<Vec<String>>(&text).unwrap_err();
        let memory = MemoryBudget::new(65536);
        let token = CancellationToken::new();
        let mut read = input(text, "a sequence", &memory, &token);
        let actual = finish(&mut read).unwrap();
        assert_eq!(actual.to_string(), reference.to_string(), "{scalar}");
        assert_eq!(actual.position().line, reference.line());
        assert_eq!(actual.position().column, reference.column());
        assert!(actual.is_data());
        assert!(!actual.is_eof());
        assert!(!actual.is_syntax());
        drop((actual, read));
        assert_eq!(memory.used(), 0);
    }
    for text in ["\n [", "\n {", "\n null", "\n 18446744073709551616"] {
        let expected = serde_json::from_str::<String>(text).unwrap_err();
        let memory = MemoryBudget::new(65536);
        let token = CancellationToken::new();
        let mut read = input(text.into(), "a string", &memory, &token);
        let actual = finish(&mut read).unwrap();
        assert_eq!(actual.to_string(), expected.to_string(), "{text}");
        drop((actual, read));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_json_type_diagnostics_own_admitted_strings_until_redaction() {
    let secret = "private\0한🙂".repeat(16384);
    let text = serde_json::to_string(&secret).unwrap();
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let mut read = input(text.clone(), "a sequence", &memory, &token);
    let actual = finish(&mut read).unwrap();
    drop(read);
    assert!(memory.used() >= secret.len());
    assert_eq!(
        actual.to_string(),
        serde_json::from_str::<Vec<String>>(&text)
            .unwrap_err()
            .to_string()
    );
    let position = actual.position();
    let redacted = actual.into_type_only();
    assert_eq!(memory.used(), 0);
    assert_eq!(redacted.position(), position);
    assert!(redacted.is_data());
    assert_eq!(
        redacted.to_string(),
        format!(
            "invalid type: string, expected a sequence at line 1 column {}",
            text.len()
        )
    );
    assert!(!format!("{redacted:?}").contains("private"));
}

#[test]
fn native_json_diagnostic_input_keeps_admission_and_cancellation_terminal() {
    for cancelled in [false, true] {
        let memory = MemoryBudget::new(8192);
        let token = CancellationToken::new();
        let mut read = input(
            format!("\"{}\"", "x".repeat(65536)),
            "a sequence",
            &memory,
            &token,
        );
        assert!(read
            .advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 }
            )
            .unwrap()
            .is_none());
        if cancelled {
            token.cancel();
        }
        assert!(matches!(
            (cancelled, finish(&mut read)),
            (true, Err(JsonInputError::Cancelled(_))) | (false, Err(JsonInputError::Memory(_)))
        ));
        assert!(read
            .advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 }
            )
            .is_err());
        assert_eq!(memory.used(), 0);
    }
}
