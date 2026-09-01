//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn key_value_column_stats_replace_is_a_complete_batch() {
    let catalog = KeyValueCatalog::new(store());
    catalog
        .save_column_stats(crate::catalog::ColumnStatsInput::basic(
            "docs", "old", 1, 0, None, None, 1,
        ))
        .unwrap();
    let replacement = [
        crate::catalog::ColumnStatsInput::basic("docs", "a", 2, 0, None, None, 3),
        crate::catalog::ColumnStatsInput::basic("docs", "b", 3, 1, None, None, 3),
    ];

    catalog.replace_column_stats("docs", &replacement).unwrap();
    assert_eq!(
        catalog
            .load_column_stats("docs")
            .unwrap()
            .into_iter()
            .map(|row| row.column_name)
            .collect::<Vec<_>>(),
        vec!["a".to_string(), "b".to_string()]
    );
}

#[test]
fn key_value_drop_cleans_only_its_legacy_public_alias() {
    let catalog = KeyValueCatalog::new(store());
    catalog.save_schema("public").unwrap();
    catalog.save_schema("app").unwrap();
    for (schema, name) in [("public", "docs"), ("app", "docs")] {
        catalog
            .save_table(&TableSchema {
                relation: crate::catalog::RelationIdentity::new(schema, name),
                object_id: [1; 16],
                storage_generation: [1; 16],
                analyzer_json: "{}".into(),
                fts_fields: Vec::new(),
                vector_fields: Vec::new(),
                columns_json: "[]".into(),
                constraints_json: String::new(),
            })
            .unwrap();
    }
    for table_name in ["public.docs", "docs", "app.docs"] {
        catalog
            .save_column_stats(crate::catalog::ColumnStatsInput::basic(
                table_name, "id", 1, 0, None, None, 1,
            ))
            .unwrap();
    }

    catalog.drop_table_and_data("public.docs").unwrap();

    assert!(catalog.load_column_stats("public.docs").unwrap().is_empty());
    assert!(catalog.load_column_stats("docs").unwrap().is_empty());
    assert_eq!(catalog.load_column_stats("app.docs").unwrap().len(), 1);
    assert_eq!(
        catalog.load_tables().unwrap()[0].relation.qualified_name(),
        "app.docs"
    );
}

#[test]
fn key_value_column_lifecycle_rejects_corrupt_catalog_index_columns() {
    let catalog = KeyValueCatalog::new(store());
    catalog
        .save_catalog_index("broken", "btree", "docs", "not-json", "{}")
        .unwrap();

    assert!(matches!(
        catalog.drop_column_data("docs", "title"),
        Err(StorageBackendError::Serde(_))
    ));
    assert_eq!(catalog.load_catalog_indexes().unwrap().len(), 1);
    assert!(matches!(
        catalog.rename_column_data("docs", "title", "headline"),
        Err(StorageBackendError::Serde(_))
    ));
    assert_eq!(
        catalog.load_catalog_indexes().unwrap()[0].columns_json,
        "not-json"
    );
}
