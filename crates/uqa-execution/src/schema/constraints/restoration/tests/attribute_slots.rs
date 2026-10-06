//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn legacy_attribute_numbers_migrate_once_and_load_only_never_repairs() {
    let catalog = catalog();
    catalog.save_table(&table("legacy")).unwrap();
    migrate_constraint_catalog(&catalog).unwrap();
    let mut row = catalog.load_tables().unwrap().remove(0);
    let mut definitions: Vec<uqa_sql::ast::ColumnDef> =
        serde_json::from_str(&row.columns_json).unwrap();
    let identity = definitions[0].object_id;
    assert_eq!(definitions[0].attribute_number, Some(1));
    definitions[0].attribute_number = None;
    row.columns_json = serde_json::to_string(&definitions).unwrap();
    catalog.save_table(&row).unwrap();
    assert!(validate_constraint_catalog(&catalog).is_err());
    assert!(migrate_constraint_catalog(&catalog).is_err());
    assert_eq!(
        catalog.load_tables().unwrap()[0].columns_json,
        row.columns_json
    );
    catalog
        .delete_metadata(RELATION_ATTRIBUTE_METADATA_KEY)
        .unwrap();
    assert!(validate_constraint_catalog(&catalog).is_err());
    migrate_constraint_catalog(&catalog).unwrap();
    validate_constraint_catalog(&catalog).unwrap();
    let restored = catalog.load_tables().unwrap().remove(0);
    let definitions: Vec<uqa_sql::ast::ColumnDef> =
        serde_json::from_str(&restored.columns_json).unwrap();
    assert_eq!(
        (definitions[0].object_id, definitions[0].attribute_number),
        (identity, Some(1))
    );
    migrate_constraint_catalog(&catalog).unwrap();
    assert_eq!(
        catalog.load_tables().unwrap()[0].columns_json,
        restored.columns_json
    );
}

#[test]
fn invalid_attribute_layout_rejects_the_catalog_before_any_migration_write() {
    let catalog = catalog();
    let first = table("first");
    let mut second = table("second");
    second.object_id = [3; 16];
    second.storage_generation = [4; 16];
    let mut definitions = columns();
    definitions[0].attribute_number = Some(2);
    second.columns_json = serde_json::to_string(&definitions).unwrap();
    catalog.save_table(&first).unwrap();
    catalog.save_table(&second).unwrap();
    let error = migrate_constraint_catalog(&catalog).unwrap_err();
    assert!(
        error.to_string().contains("attribute slot layout"),
        "{error}"
    );
    for row in catalog.load_tables().unwrap() {
        assert_eq!(
            row.columns_json.as_str(),
            if row.relation.name == "first" {
                first.columns_json.as_str()
            } else {
                second.columns_json.as_str()
            }
        );
    }
    assert!(catalog
        .get_metadata(RELATION_ATTRIBUTE_METADATA_KEY)
        .unwrap()
        .is_none());
}

#[test]
fn unknown_attribute_format_is_rejected_without_rewriting_it() {
    let catalog = catalog();
    catalog
        .set_metadata(RELATION_ATTRIBUTE_METADATA_KEY, "99")
        .unwrap();
    assert!(migrate_constraint_catalog(&catalog)
        .unwrap_err()
        .to_string()
        .contains("unsupported relation attribute format"));
    assert_eq!(
        catalog
            .get_metadata(RELATION_ATTRIBUTE_METADATA_KEY)
            .unwrap()
            .as_deref(),
        Some("99")
    );
}
