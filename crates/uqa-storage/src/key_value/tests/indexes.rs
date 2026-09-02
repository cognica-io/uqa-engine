//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn save_empty_table(catalog: &KeyValueCatalog, schema: &str, name: &str) {
    catalog.save_schema(schema).unwrap();
    catalog
        .save_table(&TableSchema {
            relation: crate::catalog::RelationIdentity::new(schema, name),
            role_owner: "uqa".into(),
            acl: None,
            column_acls: std::collections::BTreeMap::default(),
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
                role_owner: "uqa".into(),
                acl: None,
                column_acls: std::collections::BTreeMap::default(),
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
    save_empty_table(&catalog, "public", "docs");
    catalog
        .save_catalog_index(
            &crate::catalog::RelationIdentity::new("public", "broken"),
            "btree",
            "public.docs",
            "not-json",
            "{}",
        )
        .unwrap();

    assert!(matches!(
        catalog.drop_column_data("public.docs", "title"),
        Err(StorageBackendError::Serde(_))
    ));
    assert_eq!(catalog.load_catalog_indexes().unwrap().len(), 1);
    assert!(matches!(
        catalog.rename_column_data("public.docs", "title", "headline"),
        Err(StorageBackendError::Serde(_))
    ));
    assert_eq!(
        catalog.load_catalog_indexes().unwrap()[0].columns_json,
        "not-json"
    );
}

#[test]
fn key_value_catalog_indexes_enforce_schema_parent_and_shared_namespace_identity() {
    let catalog = KeyValueCatalog::new(store());
    save_empty_table(&catalog, "app", "docs");
    catalog.save_schema("archive").unwrap();
    let index = crate::catalog::RelationIdentity::new("app", "docs_idx");
    catalog
        .save_catalog_index(&index, "btree", "app.docs", "[\"id\"]", "{}")
        .unwrap();
    catalog
        .save_catalog_index(&index, "gin", "app.docs", "[\"id\"]", "{}")
        .unwrap();
    assert_eq!(catalog.load_catalog_indexes().unwrap().len(), 1);
    assert!(catalog
        .save_catalog_index(
            &crate::catalog::RelationIdentity::new("archive", "docs_idx"),
            "btree",
            "app.docs",
            "[\"id\"]",
            "{}",
        )
        .is_err());
    assert!(catalog
        .save_catalog_index(
            &crate::catalog::RelationIdentity::new("app", "missing_idx"),
            "btree",
            "app.missing",
            "[\"id\"]",
            "{}",
        )
        .is_err());
    assert!(catalog
        .save_table(&TableSchema {
            relation: crate::catalog::RelationIdentity::new("app", "docs_idx"),
            role_owner: "uqa".into(),
            acl: None,
            column_acls: std::collections::BTreeMap::default(),
            object_id: [2; 16],
            storage_generation: [2; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .is_err());
    assert!(catalog
        .save_catalog_index(
            &crate::catalog::RelationIdentity::new("app", "docs"),
            "btree",
            "app.docs",
            "[\"id\"]",
            "{}",
        )
        .is_err());

    catalog.migrate_relation_namespace().unwrap();
    catalog.drop_catalog_index(&index).unwrap();
    catalog.migrate_relation_namespace().unwrap();
    assert!(catalog.load_catalog_indexes().unwrap().is_empty());
}
