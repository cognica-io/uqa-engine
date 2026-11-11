//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::table_load::CatalogTableComponents;

fn selected(control: &StorageReadControl) -> CatalogTable {
    let mut source = source();
    for (key, bytes) in &mut source.rows {
        if key.starts_with(b"d/") {
            let mut schema: TableSchema = serde_json::from_slice(bytes).unwrap();
            schema.fts_fields = (0..8).map(|index| format!("field {index}")).collect();
            schema.vector_fields = (0..8)
                .map(|index| crate::VectorFieldSchema {
                    field: format!("vector {index}"),
                    dimensions: 3,
                })
                .collect();
            schema.analyzer_json = r#"{"tokenizer":{"type":"whitespace"}}"#.into();
            schema.columns_json = "[original columns]".into();
            schema.constraints_json = "{original constraints}".into();
            *bytes = serde_json::to_vec(&schema).unwrap();
        }
    }
    let mut rows = finish(
        &mut load(
            Arc::new(source),
            CatalogTableOrder::AfterReplacements,
            control,
        )
        .unwrap(),
        64,
    )
    .unwrap();
    rows.pop().unwrap()
}

fn input(
    table: CatalogTable,
    control: &StorageReadControl,
) -> ImageDecoder<CatalogTableComponents, StorageBackendError> {
    ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        |mut reader| async move { table.into_components_from(reader.input()).await },
    )
    .unwrap()
}

fn advance(
    input: &mut ImageDecoder<CatalogTableComponents, StorageBackendError>,
) -> StorageBackendResult<Option<CatalogTableComponents>> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn catalog_table_components_keep_original_buffers_and_independent_field_allowances() {
    let control = StorageReadControl::with_limit(1 << 22);
    let table = selected(&control);
    let original = control.memory().used();
    let relation = (table.relation.schema.as_ptr(), table.relation.name.as_ptr());
    let strings = (
        table.analyzer_json.as_ptr(),
        table.columns_json.as_ptr(),
        table.constraints_json.as_ptr(),
    );
    let fields = (table.fts_fields.as_ptr(), table.vector_fields.as_ptr());
    let expected_security = serde_json::to_value(&table.security).unwrap();
    let mut input = input(table, &control);
    let mut output = None;
    let mut calls = 0;
    let allocation = allocation_counter::measure(|| {
        while output.is_none() {
            calls += 1;
            output = advance(&mut input).unwrap();
            assert!(calls < 128);
        }
    });
    drop(input);
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    assert!(calls >= 16, "each field visit must consume a work step");
    assert_eq!(control.memory().used(), original - size_of::<TableSchema>());
    let output = output.unwrap();
    assert_eq!(
        (
            output.relation.schema.as_ptr(),
            output.relation.name.as_ptr()
        ),
        relation
    );
    assert_eq!(
        (
            output.analyzer_json.as_ptr(),
            output.columns_json.as_ptr(),
            output.constraints_json.as_ptr()
        ),
        strings
    );
    assert_eq!(
        (output.fts_fields.as_ptr(), output.vector_fields.as_ptr()),
        fields
    );
    assert_eq!(
        serde_json::to_value(&*output.security).unwrap(),
        expected_security
    );
    assert_eq!(output.object_id, [1; 16]);
    assert_eq!(output.storage_generation, [2; 16]);
    let columns_charge = output.columns_json.reserved_bytes();
    assert_eq!(columns_charge, output.columns_json.capacity());
    let before = control.memory().used();
    drop(output.columns_json);
    assert_eq!(control.memory().used(), before - columns_charge);
    let retained = output.fts_fields;
    let retained_bytes = retained.reserved_bytes();
    drop((
        output.relation,
        output.security,
        output.analyzer_json,
        output.vector_fields,
        output.constraints_json,
    ));
    assert_eq!(control.memory().used(), retained_bytes);
    assert_eq!(retained[7], "field 7");
    drop(retained);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn catalog_table_components_retire_the_original_table_at_every_pending_boundary() {
    let control = StorageReadControl::with_limit(1 << 22);
    let mut baseline = input(selected(&control), &control);
    let mut pending = 0;
    while advance(&mut baseline).unwrap().is_none() {
        pending += 1;
    }
    drop(baseline);
    assert_eq!(control.memory().used(), 0);
    for stop in 0..=pending {
        for cancel in [false, true] {
            let control = StorageReadControl::with_limit(1 << 22);
            let mut input = input(selected(&control), &control);
            for _ in 0..stop {
                assert!(advance(&mut input).unwrap().is_none());
            }
            if cancel {
                control.cancellation().cancel();
                assert!(advance(&mut input).is_err());
            }
            drop(input);
            assert_eq!(control.memory().used(), 0, "stop {stop}, cancel {cancel}");
        }
    }
}

#[test]
fn catalog_table_components_reject_an_unrelated_allowance_and_release_both_owners() {
    let original = StorageReadControl::with_limit(1 << 22);
    let other = StorageReadControl::with_limit(1 << 22);
    let mut input = input(selected(&original), &other);
    let error = loop {
        match advance(&mut input) {
            Ok(None) => (),
            Ok(Some(_)) => panic!("unrelated input allowance accepted"),
            Err(error) => break error,
        }
    };
    assert!(
        matches!(error, StorageBackendError::Other(ref message) if message == "table components require their original input allowance")
    );
    drop(input);
    assert_eq!(original.memory().used(), 0);
    assert_eq!(other.memory().used(), 0);
}
