//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn native_diskann_catalog_binding_rejects_invalid_fields_parameters_and_controls() {
    let connection = memory();
    let catalog = setup(&connection);
    let control = StorageReadControl::with_limit(1 << 20);
    let index = canonical(&connection, TABLE, FIELD, 2);
    for fault in 0..4 {
        let mut changed = row();
        match fault {
            0 => changed.index_type = "hnsw".into(),
            1 => changed.columns_json = "[\"missing\"]".into(),
            2 => changed.columns_json = "[\"vector\",\"vector\"]".into(),
            _ => changed.parameters_json = "{}".into(),
        }
        catalog.save_catalog_index_row(&changed).unwrap();
        assert!(index.retain_for_index(&row().relation, &control).is_err());
    }
    catalog.save_catalog_index_row(&row()).unwrap();
    assert!(canonical(&connection, TABLE, FIELD, 3)
        .retain_for_index(&row().relation, &control)
        .is_err());
    let tiny = StorageReadControl::with_limit(1);
    assert!(index.retain_for_index(&row().relation, &tiny).is_err());
    assert_eq!(tiny.memory().used(), 0);
    let source = capture(&connection, &control);
    let query = StorageReadControl::with_limit(8192);
    query.cancellation().cancel();
    assert!(guard(&connection, &source, &query).is_err());
    control.cancellation().cancel();
    assert!(guard(&connection, &source, &StorageReadControl::with_limit(8192)).is_err());
}

#[test]
fn native_diskann_catalog_binding_rejects_oversized_catalog_before_decoding() {
    let connection = memory();
    let catalog = setup(&connection);
    let mut definition = row();
    definition.definition_json = Some(" ".repeat(128 << 10));
    catalog.save_catalog_index_row(&definition).unwrap();
    let control = StorageReadControl::with_limit(16 << 10);
    let error = canonical(&connection, TABLE, FIELD, 2)
        .retain_for_index(&definition.relation, &control)
        .err()
        .expect("oversized metadata rejected");
    assert!(matches!(error, uqa_storage::StorageBackendError::Memory(_)));
    assert!(control.memory().peak() < 128 << 10);
    assert_eq!(control.memory().used(), 0);
}
