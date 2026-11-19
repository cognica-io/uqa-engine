//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

#[test]
fn catalog_analyzer_ordering_keeps_complete_tuples_and_resumes_common_prefixes() {
    let control = StorageReadControl::with_limit(4 << 20);
    let prefix = "한🙂".repeat(8192);
    let mut expected_bindings = Vec::new();
    let mut expected_labels = Vec::new();
    for (table, field, phase, name) in [
        ("b", "a", "both", "c"),
        ("a", "b", "both", "b"),
        ("a", "a", "search", "b"),
        ("a", "a", "both", "c"),
        ("a", "a", "both", "a"),
        ("a", "a", "both", "a"),
    ] {
        expected_bindings.push((
            format!("{prefix}{table}"),
            field.to_owned(),
            name.to_owned(),
        ));
        expected_labels.push((
            format!("{prefix}{table}"),
            field.to_owned(),
            phase.to_owned(),
            name.to_owned(),
        ));
    }
    let mut bindings: Vec<_> = expected_bindings
        .iter()
        .map(|(table, field, json)| {
            CatalogAnalyzerBindingRow::copy(table, field, json, &control).unwrap()
        })
        .collect();
    let mut labels: Vec<_> = expected_labels
        .iter()
        .map(|(table, field, phase, name)| {
            CatalogAnalyzerLabelRow::copy(table, field, phase, name, &control).unwrap()
        })
        .collect();
    let mut original_addresses: Vec<_> = bindings.iter().map(|row| row.table.as_ptr()).collect();
    original_addresses.sort();
    expected_bindings.sort();
    expected_labels.sort();
    let row_bytes = control.memory().used();
    let mut input = ImageDecoder::<_, StorageBackendError>::new(
        control.memory(),
        control.cancellation(),
        |mut reader| async move {
            let mut work = reader.input();
            CatalogAnalyzerBindingRow::order_from(&mut bindings, &mut work).await?;
            CatalogAnalyzerLabelRow::order_from(&mut labels, &mut work).await?;
            Ok((bindings, labels))
        },
    )
    .unwrap();
    let mut polls = 0;
    let (bindings, labels) = loop {
        polls += 1;
        if let Some(rows) = input
            .advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 },
            )
            .unwrap()
        {
            break rows;
        }
    };
    assert!(polls > 128);
    assert_eq!(control.memory().used(), row_bytes);
    for (actual, expected) in bindings.iter().zip(&expected_bindings) {
        assert_eq!(
            (&actual.table, &actual.field, &actual.binding_json),
            (&expected.0, &expected.1, &expected.2)
        );
    }
    for (actual, expected) in labels.iter().zip(&expected_labels) {
        assert_eq!(
            (
                &actual.table,
                &actual.field,
                &actual.phase,
                &actual.analyzer_name
            ),
            (&expected.0, &expected.1, &expected.2, &expected.3)
        );
    }
    let mut addresses: Vec<_> = bindings.iter().map(|row| row.table.as_ptr()).collect();
    addresses.sort();
    assert_eq!(addresses, original_addresses);
    drop((bindings, labels, input));
    assert_eq!(control.memory().used(), 0);
}
