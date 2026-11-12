//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn failure<T: Fixture>(
    catalog: Arc<dyn CatalogFacade>,
    limits: CatalogInputLimits,
) -> StorageBackendError {
    let control = StorageReadControl::with_limit(4 << 20);
    let mut input = new::<T>(
        &T::FORMAT,
        catalog,
        roles(),
        limits,
        control.memory(),
        &control,
    )
    .unwrap();
    let error = finish(&mut input, 1).err().expect("invalid type catalog");
    assert!(input.advance(1).is_err());
    assert_eq!(control.memory().used(), 0);
    error
}

pub(super) fn malformed<T: Fixture>() {
    for corruption in [
        "missing marker",
        "marker",
        "missing claim",
        "extra claim",
        "wrong claim",
        "leading zero",
        "overflow",
        "negative",
        "zero identity",
        "late syntax",
        "claim before identity",
        "marker before syntax",
    ] {
        let catalog = fixture::<T>(2);
        let format = &T::FORMAT;
        let (first_name, definition) = T::sample(0);
        let first_key = format.key(&first_name);
        let first_claim = format!("{}{}", format.oid_prefix, definition.oid_at(0));
        let mut value = serde_json::to_value(definition).unwrap();
        match corruption {
            "missing marker" => catalog.delete_metadata(format.format_key).unwrap(),
            "marker" => catalog
                .set_metadata(
                    format.format_key,
                    &format!(r#"{{"{}":2}}"#, format.format_field),
                )
                .unwrap(),
            "missing claim" => catalog.delete_metadata(&first_claim).unwrap(),
            "extra claim" => catalog
                .set_metadata(&format!("{}999999", format.oid_prefix), &first_name)
                .unwrap(),
            "wrong claim" => catalog.set_metadata(&first_claim, "public.other").unwrap(),
            "leading zero" | "overflow" | "negative" => {
                catalog.delete_metadata(&first_claim).unwrap();
                let suffix = match corruption {
                    "leading zero" => "020000",
                    "overflow" => "4294967296",
                    _ => "-1",
                };
                catalog
                    .set_metadata(&format!("{}{suffix}", format.oid_prefix), &first_name)
                    .unwrap();
            }
            "marker before syntax" => {
                catalog.set_metadata(&first_key, "bad JSON").unwrap();
                catalog
                    .set_metadata(
                        format.format_key,
                        &format!(r#"{{"{}":2}}"#, format.format_field),
                    )
                    .unwrap();
            }
            _ => {
                value["object_id"] = serde_json::to_value([0u8; 16]).unwrap();
                catalog
                    .set_metadata(&first_key, &value.to_string())
                    .unwrap();
                if corruption == "late syntax" {
                    catalog
                        .set_metadata(&format.key(&T::sample(1).0), "bad JSON")
                        .unwrap();
                }
                if corruption == "claim before identity" {
                    catalog.delete_metadata(&first_claim).unwrap();
                }
            }
        }
        let expected = T::restore(catalog.as_ref(), &roles())
            .err()
            .expect("ordinary failure");
        let actual = failure::<T>(catalog, LIMITS);
        match expected {
            StorageBackendError::Serde(_) => assert!(
                matches!(actual, StorageBackendError::Serde(_)),
                "{corruption}: {actual}"
            ),
            expected => assert_eq!(actual.to_string(), expected.to_string(), "{corruption}"),
        }
    }
}

#[test]
fn persistent_type_input_preserves_marker_claim_and_complete_semantic_error_order() {
    malformed::<StoredEnum>();
    malformed::<StoredShellType>();
}

#[test]
fn persistent_type_input_finishes_both_physical_families_before_marker_and_definitions() {
    let catalog = fixture::<StoredEnum>(2);
    let format = &StoredEnum::FORMAT;
    catalog
        .set_metadata(&format.key(&StoredEnum::sample(0).0), "bad definition JSON")
        .unwrap();
    catalog
        .set_metadata(format.format_key, "bad marker JSON")
        .unwrap();
    let later_claim = format!("{}999999", format.oid_prefix);
    catalog
        .set_metadata(&later_claim, &"x".repeat(4096))
        .unwrap();
    let error = failure::<StoredEnum>(
        Arc::clone(&catalog),
        CatalogInputLimits {
            maximum_value_bytes: 2048,
            ..LIMITS
        },
    );
    assert!(!matches!(error, StorageBackendError::Serde(_)), "{error}");
    catalog.delete_metadata(&later_claim).unwrap();
    assert!(matches!(
        failure::<StoredEnum>(Arc::clone(&catalog), LIMITS),
        StorageBackendError::Serde(_)
    ));
    catalog
        .set_metadata(format.format_key, &format.marker())
        .unwrap();
    assert!(matches!(
        failure::<StoredEnum>(catalog, LIMITS),
        StorageBackendError::Serde(_)
    ));
}

#[test]
fn persistent_type_claims_use_complete_canonical_name_order_before_duplicate_oid_validation() {
    let catalog = fixture::<StoredEnum>(2);
    let format = &StoredEnum::FORMAT;
    let (first_name, mut first) = StoredEnum::sample(0);
    let (last_name, last) = StoredEnum::sample(1);
    let original_oid = first.labels[0].oid;
    first.labels[0].oid = last.oid;
    catalog
        .set_metadata(
            &format.key(&first_name),
            &serde_json::to_string(&first).unwrap(),
        )
        .unwrap();
    catalog
        .delete_metadata(&format!("{}{original_oid}", format.oid_prefix))
        .unwrap();
    let claim = format!("{}{}", format.oid_prefix, last.oid);
    for name in [first_name, last_name] {
        catalog.set_metadata(&claim, &name).unwrap();
        let expected = StoredEnum::restore(catalog.as_ref(), &roles())
            .err()
            .unwrap();
        let actual = failure::<StoredEnum>(Arc::clone(&catalog), LIMITS);
        assert_eq!(actual.to_string(), expected.to_string());
    }
}
