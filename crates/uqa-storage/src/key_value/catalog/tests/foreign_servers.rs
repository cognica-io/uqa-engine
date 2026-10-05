//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn foreign_server_rows_decode_legacy_values_and_preserve_metadata_on_legacy_updates() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let catalog = KeyValueCatalog::new(store.clone());
    store
        .put(
            &single_str_key(TAG_FOREIGN_SERVER, "legacy").unwrap(),
            &encode_value(&serde_json::json!({"fdw_type":"memory","options_json":"{}"})).unwrap(),
        )
        .unwrap();
    let mut row = ForeignServerRow {
        name: "legacy".into(),
        fdw_type: "memory".into(),
        options_json: "{}".into(),
        metadata_json: None,
    };
    assert_eq!(catalog.load_foreign_server_rows().unwrap(), [row.clone()]);
    row.metadata_json = Some("opaque metadata".into());
    catalog.save_foreign_server_row(&row).unwrap();
    catalog
        .save_foreign_server("legacy", "replacement", "changed")
        .unwrap();
    row.fdw_type = "replacement".into();
    row.options_json = "changed".into();
    assert_eq!(catalog.load_foreign_server_rows().unwrap(), [row.clone()]);
    row.metadata_json = None;
    catalog.save_foreign_server_row(&row).unwrap();
    assert_eq!(catalog.load_foreign_server_rows().unwrap(), [row]);
    catalog.drop_foreign_server("legacy").unwrap();
    assert!(catalog.load_foreign_server_rows().unwrap().is_empty());
}
