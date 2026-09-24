//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn referencing_table(name: &str) -> TableSchema {
    let uqa_sql::Statement::CreateTable(declaration) = uqa_sql::compile("CREATE TABLE child(v integer CONSTRAINT inline_fk REFERENCES parent(id), w integer, CONSTRAINT table_fk FOREIGN KEY(w) REFERENCES parent(id))").unwrap().remove(0) else { panic!("table declaration") };
    let mut row = table(name);
    row.columns_json = serde_json::to_string(&declaration.columns).unwrap();
    row.constraints_json = serde_json::to_string(&uqa_sql::ast::TableConstraintSet {
        foreign_keys: declaration.foreign_keys,
        ..Default::default()
    })
    .unwrap();
    row
}

#[test]
fn foreign_key_migration_preserves_both_legacy_forms_and_is_read_only_after_conversion() {
    let catalog = catalog();
    let original = referencing_table("child");
    catalog.save_table(&original).unwrap();
    migrate_constraint_catalog(&catalog).unwrap();
    validate_constraint_catalog(&catalog).unwrap();
    let migrated = catalog.load_tables().unwrap().remove(0);
    let columns: Vec<uqa_sql::ast::ColumnDef> =
        serde_json::from_str(&migrated.columns_json).unwrap();
    let constraints: uqa_sql::ast::TableConstraintSet =
        serde_json::from_str(&migrated.constraints_json).unwrap();
    for (identity, name) in foreign_keys::identities(&columns, &constraints)
        .flatten()
        .zip(["inline_fk", "table_fk"])
    {
        assert_eq!(
            identity.oid,
            uqa_sql::catalog::oids::stable_oid("constraint", &format!("public.child.{name}"))
        );
    }
    migrate_constraint_catalog(&catalog).unwrap();
    let reopened = catalog.load_tables().unwrap().remove(0);
    assert_eq!(reopened.columns_json, migrated.columns_json);
    assert_eq!(reopened.constraints_json, migrated.constraints_json);
    assert_eq!(
        catalog
            .get_metadata(FOREIGN_KEY_IDENTITY_METADATA_KEY)
            .unwrap()
            .as_deref(),
        Some("1")
    );
}

#[test]
fn current_foreign_key_identity_loss_never_becomes_a_legacy_repair() {
    let catalog = catalog();
    let original = referencing_table("child");
    catalog.save_table(&original).unwrap();
    catalog
        .set_metadata(FOREIGN_KEY_IDENTITY_METADATA_KEY, "1")
        .unwrap();
    assert!(migrate_constraint_catalog(&catalog)
        .unwrap_err()
        .to_string()
        .contains("initial catalog identity migration"));
    let unchanged = catalog.load_tables().unwrap().remove(0);
    assert_eq!(unchanged.columns_json, original.columns_json);
    assert_eq!(unchanged.constraints_json, original.constraints_json);
    assert!(catalog
        .get_metadata(IDENTITY_METADATA_KEY)
        .unwrap()
        .is_none());
}

#[test]
fn foreign_key_and_not_null_identity_collisions_reject_the_complete_candidate() {
    let catalog = catalog();
    let mut ordinary = table("ordinary");
    let mut ordinary_columns = columns();
    let identity = uqa_sql::ast::ConstraintCatalogIdentity {
        object_id: [88; 16],
        oid: 21_000,
    };
    ordinary_columns[0].not_null_identity = Some(identity);
    ordinary.columns_json = serde_json::to_string(&ordinary_columns).unwrap();
    let mut referencing = referencing_table("referencing");
    let mut referencing_columns: Vec<uqa_sql::ast::ColumnDef> =
        serde_json::from_str(&referencing.columns_json).unwrap();
    referencing_columns[0]
        .references
        .as_mut()
        .unwrap()
        .catalog_identity = Some(identity);
    referencing.columns_json = serde_json::to_string(&referencing_columns).unwrap();
    catalog.save_table(&ordinary).unwrap();
    catalog.save_table(&referencing).unwrap();
    assert!(migrate_constraint_catalog(&catalog)
        .unwrap_err()
        .to_string()
        .contains("duplicate"));
    assert_eq!(
        catalog
            .load_tables()
            .unwrap()
            .into_iter()
            .find(|row| row.relation.name == "referencing")
            .unwrap()
            .columns_json,
        referencing.columns_json
    );
    assert!(catalog
        .get_metadata(FOREIGN_KEY_IDENTITY_METADATA_KEY)
        .unwrap()
        .is_none());
}
