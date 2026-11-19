//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::RelationIdentity;
use uqa_storage::RelationSecurityRow;

mod compatibility;

fn row() -> ForeignTableRow {
    let definition = crate::catalog::foreign::capture::tests::definition();
    ForeignTableRow {
        relation: RelationIdentity::new("Quoted.Schema", "Foreign.Name"),
        security: RelationSecurityRow::bootstrap(),
        server_name: definition.server_name.clone(),
        columns_json: definition.schema_json().unwrap(),
        options_json: serde_json::to_string(&definition.options).unwrap(),
    }
}

fn compare(row: &ForeignTableRow, format: Option<u32>) -> bool {
    let expected = StoredForeignTable::from_catalog_row(row, format);
    let control = StorageReadControl::with_limit(1 << 26);
    let invoking = CancellationToken::new();
    let actual =
        StoredForeignTable::from_catalog_row_with_control(row, format, &control, &invoking);
    let valid = expected.is_ok();
    match (expected, actual) {
        (Ok((expected, legacy)), Ok(actual)) => {
            assert_eq!(actual.1, legacy);
            assert_eq!(actual.0.name, expected.name);
            assert_eq!(actual.0.server_name, expected.server_name);
            assert_eq!(actual.0.persistence, expected.persistence);
            assert_eq!(
                actual.0.schema_json().unwrap(),
                expected.schema_json().unwrap()
            );
            assert!(actual.0.options.iter().eq(expected.options.iter()));
            assert_eq!(actual.reserved_bytes(), control.memory().used());
        }
        (Err(expected), Err(actual)) => {
            assert_eq!(
                std::mem::discriminant(&expected),
                std::mem::discriminant(&actual),
                "{}: {expected} / {actual}",
                row.columns_json
            );
            if matches!(expected, StorageBackendError::Other(_)) {
                assert_eq!(expected.to_string(), actual.to_string());
            }
        }
        (expected, actual) => panic!("{}: {expected:?} / {actual:?}", row.columns_json),
    }
    assert_eq!(control.memory().used(), 0);
    valid
}

#[test]
fn stored_foreign_input_preserves_complete_declarations_and_original_option_order() {
    let mut row = row();
    assert!(compare(&row, Some(2)));
    row.options_json = r#"{"z":"overwritten","a":"second 한글","z":"first\nvalue"}"#.into();
    assert!(compare(&row, Some(2)));
    let schema: serde_json::Value = serde_json::from_str(&row.columns_json).unwrap();
    for name in schema.as_object().unwrap().keys() {
        let mut missing = schema.clone();
        missing.as_object_mut().unwrap().remove(name);
        row.columns_json = missing.to_string();
        compare(&row, None);
        let mut null = schema.clone();
        null[name] = serde_json::Value::Null;
        row.columns_json = null.to_string();
        compare(&row, None);
    }
    row.columns_json = schema["columns"].to_string();
    assert!(compare(&row, None));
    assert!(!compare(&row, Some(2)));
    row.columns_json = "[]".into();
    assert!(compare(&row, None));
}

#[test]
fn stored_foreign_input_retains_exact_allocations_after_source_retirement() {
    let mut row = row();
    row.columns_json = row.columns_json.replace("payload", &"x".repeat(65536));
    let control = StorageReadControl::with_limit(1 << 24);
    let invoking = CancellationToken::new();
    let mut result = None;
    let allocations = allocation_counter::measure(|| {
        result = Some(
            StoredForeignTable::from_catalog_row_with_control(&row, Some(2), &control, &invoking)
                .unwrap(),
        );
    });
    assert_eq!(
        usize::try_from(allocations.bytes_current).unwrap(),
        control.memory().used()
    );
    assert!(
        usize::try_from(allocations.bytes_max).unwrap() <= control.memory().peak(),
        "{allocations:?} / {}",
        control.memory().peak()
    );
    assert_eq!(
        result.as_ref().unwrap().reserved_bytes(),
        control.memory().used()
    );
    let expected = StoredForeignTable::from_catalog_row(&row, Some(2))
        .unwrap()
        .0
        .schema_json()
        .unwrap();
    let peak = control.memory().peak();
    drop(row);
    control.cancellation().cancel();
    invoking.cancel();
    assert_eq!(result.as_ref().unwrap().0.schema_json().unwrap(), expected);
    drop(result);
    assert_eq!(control.memory().used(), 0);
    let mut row = self::row();
    row.columns_json = row.columns_json.replace("payload", &"x".repeat(65536));
    for limit in (0..peak).step_by(1997).chain([peak - 1]) {
        let control = StorageReadControl::with_limit(limit);
        let invoking = CancellationToken::new();
        assert!(StoredForeignTable::from_catalog_row_with_control(
            &row,
            Some(2),
            &control,
            &invoking
        )
        .is_err());
        assert_eq!(control.memory().used(), 0);
        assert!(control.memory().peak() <= limit);
    }
}

#[test]
fn stored_foreign_input_honors_both_cancellation_owners_and_borrows_discarded_numbers() {
    let row = row();
    for original in [false, true] {
        let control = StorageReadControl::with_limit(1 << 20);
        let invoking = CancellationToken::new();
        if original {
            control.cancellation().cancel();
        } else {
            invoking.cancel();
        }
        assert!(matches!(
            StoredForeignTable::from_catalog_row_with_control(&row, None, &control, &invoking),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(control.memory().peak(), 0);
    }
    let mut row = row;
    row.columns_json = format!(
        r#"{{"unknown":{},"version":1,"columns":[]}}"#,
        "9".repeat(2 << 20)
    );
    assert!(compare(&row, None));
    let control = StorageReadControl::with_limit(8192);
    let invoking = CancellationToken::new();
    let allocations = allocation_counter::measure(|| {
        drop(
            StoredForeignTable::from_catalog_row_with_control(&row, None, &control, &invoking)
                .unwrap(),
        );
    });
    assert_eq!(control.memory().used(), 0);
    assert!(allocations.bytes_max < 8192, "{allocations:?}");
}
