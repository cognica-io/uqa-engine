//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ast::{ConstraintCatalogIdentity, TableConstraintSet};

fn key_schema(name: &str) -> TableSchema {
    let uqa_sql::Statement::CreateTable(declaration) = uqa_sql::compile("CREATE TABLE t(v int CONSTRAINT required NOT NULL, CONSTRAINT k UNIQUE(v), CONSTRAINT positive CHECK(v > 0))").unwrap().remove(0) else { panic!("table declaration") };
    let mut schema = table(name);
    schema.columns_json = serde_json::to_string(&declaration.columns).unwrap();
    schema.constraints_json = serde_json::to_string(&TableConstraintSet {
        key_constraints: declaration.key_constraints,
        checks: declaration.checks,
        ..Default::default()
    })
    .unwrap();
    schema
}

#[test]
fn key_and_check_addresses_migrate_once_and_current_missing_addresses_are_rejected() {
    let catalog = catalog();
    catalog.save_table(&key_schema("ordinary")).unwrap();
    migrate_constraint_catalog(&catalog).unwrap();
    validate_constraint_catalog(&catalog).unwrap();
    let row = catalog.load_tables().unwrap().remove(0);
    let mut constraints: TableConstraintSet = serde_json::from_str(&row.constraints_json).unwrap();
    assert_eq!(
        constraints.key_constraints[0].catalog_identity.unwrap().oid,
        uqa_sql::catalog::oids::stable_oid("constraint", "public.ordinary.k")
    );
    assert_eq!(
        constraints.checks[0].catalog_oid,
        Some(uqa_sql::catalog::oids::stable_oid(
            "constraint",
            "public.ordinary.positive"
        ))
    );
    migrate_constraint_catalog(&catalog).unwrap();
    assert_eq!(
        catalog.load_tables().unwrap()[0].constraints_json,
        row.constraints_json
    );
    constraints.key_constraints[0].catalog_identity = None;
    let mut invalid = row.clone();
    invalid.constraints_json = serde_json::to_string(&constraints).unwrap();
    catalog.save_table(&invalid).unwrap();
    assert!(migrate_constraint_catalog(&catalog)
        .unwrap_err()
        .to_string()
        .contains("key constraints require"));
    assert!(validate_constraint_catalog(&catalog).is_err());
    assert_eq!(
        catalog.load_tables().unwrap()[0].constraints_json,
        invalid.constraints_json
    );
    assert_eq!(
        catalog
            .get_metadata(CATALOG_ADDRESS_METADATA_KEY)
            .unwrap()
            .as_deref(),
        Some("1")
    );
}

#[test]
fn legacy_key_collision_is_reassigned_without_changing_a_current_not_null_address() {
    let catalog = catalog();
    let mut row = key_schema("ordinary");
    let mut columns: Vec<uqa_sql::ast::ColumnDef> =
        serde_json::from_str(&row.columns_json).unwrap();
    let occupied = uqa_sql::catalog::oids::stable_oid("constraint", "public.ordinary.k");
    columns[0].not_null_identity = Some(ConstraintCatalogIdentity {
        object_id: [77; 16],
        oid: occupied,
    });
    row.columns_json = serde_json::to_string(&columns).unwrap();
    catalog.save_table(&row).unwrap();
    catalog.set_metadata(IDENTITY_METADATA_KEY, "1").unwrap();
    migrate_constraint_catalog(&catalog).unwrap();
    validate_constraint_catalog(&catalog).unwrap();
    let current = catalog.load_tables().unwrap().remove(0);
    let current_columns: Vec<uqa_sql::ast::ColumnDef> =
        serde_json::from_str(&current.columns_json).unwrap();
    let constraints: TableConstraintSet = serde_json::from_str(&current.constraints_json).unwrap();
    assert_eq!(
        current_columns[0].not_null_identity,
        columns[0].not_null_identity
    );
    assert_ne!(
        constraints.key_constraints[0].catalog_identity.unwrap().oid,
        occupied
    );
}

#[test]
fn foreign_check_corruption_prevents_every_conversion_write() {
    let catalog = catalog();
    let ordinary = key_schema("ordinary");
    catalog.save_table(&ordinary).unwrap();
    let mut columns = columns();
    columns[0].check_catalog_oid = Some(26_000);
    let foreign = uqa_storage::ForeignTableRow {
        relation: RelationIdentity::new("public", "foreign_relation"),
        security: RelationSecurityRow::legacy("uqa"),
        server_name: "memory".into(),
        columns_json: serde_json::to_string(&columns).unwrap(),
        options_json: "{}".into(),
    };
    catalog.save_foreign_table(&foreign).unwrap();
    assert!(migrate_constraint_catalog(&catalog)
        .unwrap_err()
        .to_string()
        .contains("without CHECK"));
    assert_eq!(
        catalog.load_tables().unwrap()[0].columns_json,
        ordinary.columns_json
    );
    assert_eq!(
        catalog.load_tables().unwrap()[0].constraints_json,
        ordinary.constraints_json
    );
    assert_eq!(
        catalog.load_foreign_tables().unwrap()[0].columns_json,
        foreign.columns_json
    );
    assert!(catalog
        .get_metadata(CATALOG_ADDRESS_METADATA_KEY)
        .unwrap()
        .is_none());
}
