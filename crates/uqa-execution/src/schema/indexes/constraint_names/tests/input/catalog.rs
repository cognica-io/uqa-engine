//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};
use uqa_storage::{catalog::input::CatalogInputLimits, read_control::StorageReadControl};

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 4096,
    maximum_value_bytes: 1 << 20,
};

fn catalog(version: Option<&str>, invalid: bool) -> Arc<dyn CatalogFacade> {
    use uqa_storage::PersistentStorageProvider;
    let provider = uqa_storage_sqlite::SQLiteKeyValueStorage::open_in_memory().unwrap();
    let session = provider.open_session().unwrap();
    let catalog = session.catalog;
    catalog.initialize_storage().unwrap();
    catalog
        .save_schema_row(&uqa_storage::SchemaRow::bootstrap("public"))
        .unwrap();
    if let Some(version) = version {
        catalog.set_metadata(REGISTRY_VERSION, version).unwrap();
    }
    let (schema, _, _) = fixture();
    catalog.save_table(&schema).unwrap();
    let mut index = row();
    if invalid {
        index.definition_json = Some("invalid definition".into());
    }
    catalog.save_catalog_index_row(&index).unwrap();
    catalog
}

fn input(
    catalog: Arc<dyn CatalogFacade>,
    control: &StorageReadControl,
    invoking: &CancellationToken,
) -> ImageDecoder<KeyConstraintNames, StorageBackendError> {
    let retained = control.clone();
    ImageDecoder::new(control.memory(), invoking, |mut reader| async move {
        KeyConstraintNames::load_from(&*catalog, LIMITS, &retained, &mut reader.input()).await
    })
    .unwrap()
}

fn finish(
    input: &mut ImageDecoder<KeyConstraintNames, StorageBackendError>,
) -> StorageBackendResult<KeyConstraintNames> {
    for _ in 0..4096 {
        if let Some(names) = input.advance(
            &mut std::io::empty(),
            ImageDecodeWindow { bytes: 1, steps: 1 },
        )? {
            return Ok(names);
        }
    }
    panic!("catalog key names did not complete");
}

#[test]
fn retained_key_name_catalog_preserves_version_selection_and_current_identity() {
    for version in [None, Some("1"), Some("2")] {
        let catalog = catalog(version, false);
        let control = StorageReadControl::with_limit(1 << 20);
        let invoking = CancellationToken::new();
        let names = finish(&mut input(catalog.clone(), &control, &invoking)).unwrap();
        let (mut schema, expected, _) = fixture();
        schema.constraints_json = if version == Some("2") {
            encode(&expected).unwrap()
        } else {
            serde_json::to_string(&expected).unwrap()
        };
        let ordinary = KeyConstraintNames::load(&*catalog)
            .unwrap()
            .decode(&schema)
            .unwrap();
        let retained = names
            .decode_stored_with_control(
                &schema,
                control.memory(),
                control.cancellation(),
                &invoking,
            )
            .unwrap();
        assert_eq!(
            serde_json::to_value(&*retained).unwrap(),
            serde_json::to_value(ordinary).unwrap()
        );
        drop((names, retained));
        assert_eq!(control.memory().used(), 0);
    }
    for version in [None, Some("1"), Some("2"), Some("3")] {
        let control = StorageReadControl::with_limit(1 << 20);
        let invoking = CancellationToken::new();
        let result = finish(&mut input(catalog(version, true), &control, &invoking));
        assert_eq!(result.is_ok(), matches!(version, None | Some("1")));
        drop(result);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn retained_key_name_catalog_retires_each_pending_owner_and_both_cancellations() {
    let catalog = catalog(Some("2"), false);
    let control = StorageReadControl::with_limit(1 << 20);
    let invoking = CancellationToken::new();
    let mut baseline = input(catalog.clone(), &control, &invoking);
    let mut pending = 0;
    while baseline
        .advance(
            &mut std::io::empty(),
            ImageDecodeWindow { bytes: 1, steps: 1 },
        )
        .unwrap()
        .is_none()
    {
        pending += 1;
        assert!(pending < 4096);
    }
    drop(baseline);
    assert!(pending > 1);
    assert_eq!(control.memory().used(), 0);
    for stop in 0..=pending {
        for cancelled in 0..3 {
            let control = StorageReadControl::with_limit(1 << 20);
            let invoking = CancellationToken::new();
            let mut input = input(catalog.clone(), &control, &invoking);
            for _ in 0..stop {
                assert!(input
                    .advance(
                        &mut std::io::empty(),
                        ImageDecodeWindow { bytes: 1, steps: 1 }
                    )
                    .unwrap()
                    .is_none());
            }
            if cancelled != 0 {
                if cancelled == 1 {
                    control.cancellation().cancel();
                } else {
                    invoking.cancel();
                }
                assert!(finish(&mut input).is_err());
            }
            drop(input);
            assert_eq!(
                control.memory().used(),
                0,
                "stop {stop}, cancellation {cancelled}"
            );
        }
    }
}
