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
fn backup_retained_catalog_diagnostics_release_and_hide_original_payloads() {
    use uqa_core::{json::JsonPosition, memory::ProductionControl};
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let secret = "private stored role 한🙂".repeat(1024);
    let cause = std::sync::Arc::new(JsonDiagnostic::unknown_field(
        control.copy_text(&secret).unwrap(),
        &["owner"],
        JsonPosition::unlocated(),
    ));
    let weak = std::sync::Arc::downgrade(&cause);
    assert!(memory.used() >= secret.len());
    let error = BackupError::from(StorageBackendError::RetainedDiagnostic(cause));
    assert!(matches!(
        error,
        BackupError::Invalid("stored catalog authority")
    ));
    assert!(!format!("{error}\n{error:?}").contains("private stored role"));
    assert!(std::error::Error::source(&error).is_none());
    assert!(weak.upgrade().is_none());
    assert_eq!(memory.used(), 0);
}

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

#[test]
fn backup_descriptor_diagnostics_retire_payloads_through_the_analysis_owner() {
    use uqa_analysis::{AnalysisError, AnalyzerResources};
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let text = format!("{{{:?}:null}}", "private descriptor field".repeat(1024));
    let mut input =
        ImageDecoder::<(), StorageBackendError>::new(&memory, &token, |mut reader| async move {
            AnalyzerResources::default()
                .restore_json_with_input(&text, &mut reader.input())
                .await?;
            panic!("unknown descriptor field accepted")
        })
        .unwrap();
    let error = loop {
        match input.advance(
            &mut std::io::empty(),
            ImageDecodeWindow { bytes: 1, steps: 1 },
        ) {
            Ok(None) => (),
            Err(error) => break error,
            Ok(Some(())) => unreachable!(),
        }
    };
    drop(input);
    let StorageBackendError::Analysis(AnalysisError::JsonDiagnostic(ref diagnostic)) = error else {
        panic!("analysis diagnostic lost: {error}")
    };
    let position = diagnostic.position();
    assert!(memory.used() >= 20_000);
    assert!(error.to_string().contains("private descriptor field"));
    let backup = BackupError::from(error);
    assert_eq!(memory.used(), 0);
    assert!(!format!("{backup:?} {backup}").contains("private descriptor field"));
    let BackupError::Storage(StorageBackendError::Analysis(AnalysisError::JsonDiagnostic(error))) =
        backup
    else {
        panic!("analysis owner lost")
    };
    assert!(error.is_data());
    assert_eq!(error.position(), position);
}
