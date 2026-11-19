//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 4096,
    maximum_value_bytes: 1 << 20,
};

fn fixture() -> (Arc<dyn CatalogFacade>, String) {
    let storage = uqa_storage_sqlite::SQLiteKeyValueStorage::open_in_memory().unwrap();
    let catalog = Arc::new(storage.catalog());
    let text = format!(
        r#"{{"routine_catalog_format":3,"definitions":{{"f{}":[{{"body":"{}","cost":0.000001,"nested":[null,{{"x":1}}]}}],"z":[null]}}}}"#,
        "한🙂".repeat(8192),
        "text한🙂".repeat(8192)
    );
    catalog
        .set_metadata(super::super::super::FUNCTIONS_METADATA_KEY, &text)
        .unwrap();
    (catalog, text)
}

fn complete(
    input: &mut RoutineCatalogInput,
    steps: usize,
) -> StorageBackendResult<(RoutineCatalogDocument, usize)> {
    for polls in 1..1_000_000 {
        if let Some(value) = input.advance(steps)? {
            return Ok((value, polls));
        }
    }
    panic!("routine catalog input stopped progressing")
}

fn assert_document(actual: &RoutineCatalogDocument, text: &str) {
    let (format, definitions) = reference(Some(text), false).unwrap();
    assert_eq!(actual.format(), format);
    assert_eq!(actual.definitions().len(), definitions.len());
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for ((name, values), (expected_name, expected)) in actual.definitions().zip(definitions.iter())
    {
        assert_eq!(name, expected_name);
        assert_eq!(values.len(), expected.len());
        for (value, expected) in values.iter().zip(expected) {
            assert_eq!(
                value
                    .root()
                    .to_json_with_control(&control)
                    .unwrap()
                    .as_str(),
                expected.to_string()
            );
        }
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn routine_catalog_input_retains_the_selected_record_through_projection_and_source_changes() {
    let mut observations = Vec::new();
    for steps in [1, 7, 64] {
        let (catalog, text) = fixture();
        let control = StorageReadControl::with_limit(8 << 20);
        let mut input =
            RoutineCatalogInput::new(Arc::clone(&catalog), LIMITS, false, &control).unwrap();
        let initial = control.memory().used();
        assert!(input.advance(1).unwrap().is_none());
        assert!(input.advance(1).unwrap().is_none());
        assert!(control.memory().used() >= initial + text.len());
        catalog
            .set_metadata(
                super::super::super::FUNCTIONS_METADATA_KEY,
                "invalid later text",
            )
            .unwrap();
        drop(catalog);
        let (value, polls) = complete(&mut input, steps).unwrap();
        observations.push(polls);
        assert_document(&value, &text);
        assert!(input.advance(1).is_err());
        drop(input);
        drop(value);
        assert_eq!(control.memory().used(), 0);
    }
    assert!(observations[0] > observations[1] && observations[1] > observations[2]);
}

#[test]
fn routine_catalog_input_retires_cancelled_denied_and_abandoned_records() {
    let (catalog, text) = fixture();
    let baseline = StorageReadControl::with_limit(8 << 20);
    let mut input =
        RoutineCatalogInput::new(Arc::clone(&catalog), LIMITS, false, &baseline).unwrap();
    let (document, polls) = complete(&mut input, 1).unwrap();
    drop(document);
    drop(input);
    assert_eq!(baseline.memory().used(), 0);
    for boundary in [1, 2, polls / 3, polls * 2 / 3, polls - 1] {
        for cancel in [false, true] {
            let control = StorageReadControl::with_limit(8 << 20);
            let mut input =
                RoutineCatalogInput::new(Arc::clone(&catalog), LIMITS, false, &control).unwrap();
            for _ in 0..boundary {
                assert!(input.advance(1).unwrap().is_none());
            }
            if cancel {
                control.cancellation().cancel();
                assert!(matches!(
                    input.advance(1),
                    Err(StorageBackendError::Cancelled(_))
                ));
                assert!(input.advance(1).is_err());
            }
            drop(input);
            assert_eq!(control.memory().used(), 0);
        }
    }
    let mut denied = 0;
    let mut accepted = 0;
    for limit in [
        0,
        1024,
        16_384,
        65_536,
        200_000,
        400_000,
        800_000,
        1_600_000,
        8 << 20,
    ] {
        let control = StorageReadControl::with_limit(limit);
        let result = RoutineCatalogInput::new(Arc::clone(&catalog), LIMITS, false, &control)
            .and_then(|mut input| complete(&mut input, 7).map(|(value, _)| value));
        match &result {
            Ok(value) => {
                accepted += 1;
                assert_document(value, &text);
            }
            Err(StorageBackendError::Memory(_)) => denied += 1,
            Err(error) => panic!("limit {limit}: {error}"),
        }
        assert!(control.memory().peak() <= limit);
        drop(result);
        assert_eq!(control.memory().used(), 0);
    }
    assert!(denied > 0 && accepted > 0);
    let control = StorageReadControl::with_limit(8 << 20);
    let mut input = RoutineCatalogInput::new(
        Arc::clone(&catalog),
        CatalogInputLimits {
            maximum_value_bytes: 8,
            ..LIMITS
        },
        false,
        &control,
    )
    .unwrap();
    assert!(complete(&mut input, 1).is_err());
    drop(input);
    assert_eq!(control.memory().used(), 0);
    let mut input = RoutineCatalogInput::new(catalog, LIMITS, false, &control).unwrap();
    assert!(input.advance(0).is_err());
    drop(input);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn routine_catalog_input_retains_only_admitted_unknown_field_diagnostics() {
    let (catalog, _) = fixture();
    let field = format!("z{}", "한🙂".repeat(8192));
    let text = format!(r#"{{"routine_catalog_format":3,"definitions":{{}},"{field}":null}}"#);
    catalog
        .set_metadata(super::super::super::FUNCTIONS_METADATA_KEY, &text)
        .unwrap();
    let expected = reference(Some(&text), false).err().unwrap().to_string();
    let control = StorageReadControl::with_limit(8 << 20);
    let mut input = RoutineCatalogInput::new(catalog, LIMITS, false, &control).unwrap();
    let error = complete(&mut input, 1).err().unwrap();
    assert_eq!(error.to_string(), expected);
    drop(input);
    assert!(control.memory().used() >= field.len());
    let StorageBackendError::Json(error) = error else {
        panic!("admitted typed diagnostic")
    };
    assert_eq!(error.position(), JsonPosition::unlocated());
    let error = error.into_type_only();
    assert_eq!(control.memory().used(), 0);
    assert!(!error.to_string().contains(&field));
}

#[test]
fn routine_catalog_document_preserves_both_cancellation_owners() {
    let (_, text) = fixture();
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(8 << 20);
        let retained = memory.clone();
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let owner = original.clone();
        let text = text.clone();
        let mut input = ImageDecoder::<RoutineCatalogDocument, StorageBackendError>::new(
            &memory,
            &invoking,
            |mut reader| async move {
                RoutineCatalogDocument::read_from(
                    Some(&text),
                    false,
                    &retained,
                    &owner,
                    &mut reader.input(),
                )
                .await
            },
        )
        .unwrap();
        for _ in 0..100 {
            assert!(input
                .advance(
                    &mut std::io::empty(),
                    ImageDecodeWindow { bytes: 1, steps: 1 }
                )
                .unwrap()
                .is_none());
        }
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(matches!(
            input.advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 }
            ),
            Err(StorageBackendError::Cancelled(_))
        ));
        drop(input);
        assert_eq!(memory.used(), 0);
    }
}
