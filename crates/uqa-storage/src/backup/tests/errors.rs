//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::StorageBackendError;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder, ImageIoError},
    json::{JsonDiagnostic, JsonInputError, JsonReader},
    memory::{MemoryBudget, MemoryError},
    CancellationToken,
};

#[test]
fn backup_native_json_boundary_retires_payloads_and_preserves_diagnostic_identity() {
    for text in [r#""private\n\u0000payload""#, "true", "42", "[1,]", "["] {
        let memory = MemoryBudget::new(65536);
        let token = CancellationToken::new();
        let mut input =
            ImageDecoder::<(), JsonInputError>::new(&memory, &token, |mut reader| async move {
                let mut work = reader.input();
                let mut json = JsonReader::new(text, work.memory, work.cancellation);
                while let Some(event) = json
                    .next_event_from(&mut work)
                    .await
                    .map_err(|error| JsonInputError::from_reader(&json, error))?
                {
                    if !text.starts_with('[') {
                        return Err(JsonDiagnostic::invalid_type_from(
                            event.token,
                            json.position(),
                            "a sequence",
                            &mut work,
                        )
                        .await?
                        .into());
                    }
                }
                panic!("invalid input accepted")
            })
            .unwrap();
        let original = loop {
            match input.advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 },
            ) {
                Ok(None) => (),
                Err(JsonInputError::Diagnostic(error)) => break error,
                result => panic!("unexpected diagnostic result: {result:?}"),
            }
        };
        let position = original.position();
        let categories = (original.is_data(), original.is_syntax(), original.is_eof());
        let message = original.to_string();
        assert_eq!(memory.used() > 0, text.starts_with('"'));
        let original = StorageBackendError::from(original);
        assert_eq!(
            original.to_string(),
            format!("payload serialization failed: {message}")
        );
        let backup = BackupError::from(original);
        assert_eq!(memory.used(), 0);
        let BackupError::Storage(StorageBackendError::Json(error)) = &backup else {
            panic!("typed diagnostic lost: {backup}")
        };
        assert_eq!(error.position(), position);
        assert_eq!(
            (error.is_data(), error.is_syntax(), error.is_eof()),
            categories
        );
        let exposed = format!("{backup}\n{backup:?}");
        for value in ["private", "payload\"", "boolean `true`", "integer `42`"] {
            assert!(!exposed.contains(value), "{exposed}");
        }
        drop((backup, input));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn backup_native_json_resources_keep_original_types_through_storage() {
    let token = CancellationToken::new();
    token.cancel();
    let cancelled = token.check().unwrap_err();
    for error in [
        JsonInputError::from(MemoryError::SizeOverflow),
        JsonInputError::from(cancelled),
        JsonInputError::from(ImageIoError::Memory(MemoryError::SizeOverflow)),
    ] {
        let memory = matches!(error, JsonInputError::Memory(_));
        let result = BackupError::from(StorageBackendError::from(error));
        assert!(matches!(
            (memory, result),
            (true, BackupError::Memory(MemoryError::SizeOverflow))
                | (false, BackupError::Cancelled(_))
        ));
    }
}
