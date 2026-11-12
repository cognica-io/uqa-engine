//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{migrate_constraint_catalog, validate_constraint_catalog};
use super::*;
use uqa_core::RelationIdentity;
use uqa_storage::{ForeignTableRow, RelationSecurityRow, TableSchema};

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 4096,
    maximum_value_bytes: 1 << 20,
};

fn fixture(count: usize) -> Arc<dyn CatalogFacade> {
    let storage = uqa_storage_sqlite::SQLiteKeyValueStorage::open_in_memory().unwrap();
    let catalog = Arc::new(storage.catalog());
    catalog
        .save_schema_row(&uqa_storage::SchemaRow::bootstrap("public"))
        .unwrap();
    let uqa_sql::Statement::CreateTable(table) = uqa_sql::compile("CREATE TABLE t(v integer CONSTRAINT required NOT NULL, c integer CHECK(c > 0), UNIQUE(v), CHECK(v > 0), FOREIGN KEY(v) REFERENCES parent(v))").unwrap().remove(0) else { panic!("table declaration") };
    let constraints = TableConstraintSet {
        checks: table.checks,
        key_constraints: table.key_constraints,
        foreign_keys: table.foreign_keys,
        ..Default::default()
    };
    for index in 0..count {
        let mut object = [0; 16];
        object[..8].copy_from_slice(&(index as u64 + 1).to_le_bytes());
        catalog
            .save_table(&TableSchema {
                relation: RelationIdentity::new("public", format!("table_{index:04}")),
                security: RelationSecurityRow::legacy("uqa"),
                object_id: object,
                storage_generation: object,
                analyzer_json: "{}".into(),
                fts_fields: Vec::new(),
                vector_fields: Vec::new(),
                columns_json: serde_json::to_string(&table.columns).unwrap(),
                constraints_json: serde_json::to_string(&constraints).unwrap(),
            })
            .unwrap();
    }
    catalog
        .save_foreign_table(&ForeignTableRow {
            relation: RelationIdentity::new("public", "foreign_table"),
            security: RelationSecurityRow::legacy("uqa"),
            server_name: "source".into(),
            columns_json: serde_json::to_string(&table.columns).unwrap(),
            options_json: "{}".into(),
        })
        .unwrap();
    migrate_constraint_catalog(catalog.as_ref()).unwrap();
    catalog
}

fn finish(input: &mut ConstraintCatalogInput) -> StorageBackendResult<usize> {
    for step in 1..65536 {
        if input.advance(1)?.is_some() {
            return Ok(step);
        }
    }
    panic!("constraint input stopped progressing")
}

#[test]
fn retained_constraint_catalog_preserves_complete_rows_and_bounds_identity_visits() {
    for count in [0, 4, 32] {
        let catalog = fixture(count);
        validate_constraint_catalog(catalog.as_ref()).unwrap();
        let tables = catalog.load_tables().unwrap();
        let foreign = catalog.load_foreign_tables().unwrap();
        let metadata = catalog.metadata_with_prefix("").unwrap();
        let control = StorageReadControl::with_limit(16 << 20);
        let mut input =
            ConstraintCatalogInput::new(Arc::clone(&catalog), LIMITS, &control).unwrap();
        let steps = finish(&mut input).unwrap();
        assert!(steps > count * 16, "{count}: {steps}");
        assert!(steps < (count + 1) * 512 + 128, "{count}: {steps}");
        assert!(input.advance(1).is_err());
        drop(input);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(
            serde_json::to_string(&catalog.load_tables().unwrap()).unwrap(),
            serde_json::to_string(&tables).unwrap()
        );
        let current = catalog.load_foreign_tables().unwrap();
        assert_eq!(current.len(), foreign.len());
        for (current, previous) in current.iter().zip(&foreign) {
            assert_eq!(current.relation, previous.relation);
            assert_eq!(current.security, previous.security);
            assert_eq!(current.server_name, previous.server_name);
            assert_eq!(current.columns_json, previous.columns_json);
            assert_eq!(current.options_json, previous.options_json);
        }
        assert_eq!(catalog.metadata_with_prefix("").unwrap(), metadata);
    }
}

#[test]
fn retained_constraint_catalog_preserves_format_and_semantic_error_order() {
    for defect in [
        "not_null_format",
        "foreign_format",
        "address_format",
        "attribute_format",
        "attributes",
        "not_null",
        "foreign",
        "check",
        "dispatch",
        "global_object",
        "global_oid",
        "reference",
    ] {
        let catalog = fixture(1);
        let mut row = catalog.load_tables().unwrap().remove(0);
        let mut columns: Vec<ColumnDef> = serde_json::from_str(&row.columns_json).unwrap();
        let mut constraints: TableConstraintSet =
            serde_json::from_str(&row.constraints_json).unwrap();
        match defect {
            "not_null_format" => catalog
                .set_metadata(super::super::IDENTITY_METADATA_KEY, "2")
                .unwrap(),
            "foreign_format" => catalog
                .delete_metadata(super::super::FOREIGN_KEY_IDENTITY_METADATA_KEY)
                .unwrap(),
            "address_format" => catalog
                .set_metadata(super::super::CATALOG_ADDRESS_METADATA_KEY, "2")
                .unwrap(),
            "attribute_format" => catalog
                .delete_metadata(super::super::RELATION_ATTRIBUTE_METADATA_KEY)
                .unwrap(),
            "attributes" => {
                columns[0].attribute_number = None;
                columns[0].not_null_identity = None;
            }
            "not_null" => {
                columns[0].not_null_identity = None;
                constraints.foreign_keys[0].catalog_identity = None;
            }
            "foreign" => {
                constraints.foreign_keys[0].catalog_identity = None;
                constraints.checks[0].object_id = None;
            }
            "check" => constraints.checks[0].object_id = None,
            "dispatch" => {
                columns[0].default = Some(uqa_sql::ast::Expr::Between {
                    expr: Box::new(uqa_sql::ast::Expr::Literal(uqa_core::Value::Int(1))),
                    low: Box::new(uqa_sql::ast::Expr::Literal(uqa_core::Value::Int(0))),
                    high: Box::new(uqa_sql::ast::Expr::Literal(uqa_core::Value::Int(2))),
                });
            }
            "global_object" | "global_oid" => {
                let mut foreign = catalog.load_foreign_tables().unwrap().remove(0);
                let (mut table, _) =
                    crate::catalog::foreign::StoredForeignTable::from_catalog_row(&foreign, None)
                        .unwrap();
                let source = columns[0].not_null_identity.unwrap();
                let target = table.columns[0].not_null_identity.as_mut().unwrap();
                if defect == "global_object" {
                    target.object_id = source.object_id;
                } else {
                    target.oid = source.oid;
                }
                foreign.columns_json = table.schema_json().unwrap();
                catalog.save_foreign_table(&foreign).unwrap();
            }
            "reference" => catalog
                .set_metadata("foreign-table-server-reference-format", "{}")
                .unwrap(),
            _ => unreachable!(),
        }
        row.columns_json = serde_json::to_string(&columns).unwrap();
        row.constraints_json = serde_json::to_string(&constraints).unwrap();
        catalog.save_table(&row).unwrap();
        let expected = validate_constraint_catalog(catalog.as_ref())
            .unwrap_err()
            .to_string();
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input =
            ConstraintCatalogInput::new(Arc::clone(&catalog), LIMITS, &control).unwrap();
        assert_eq!(
            finish(&mut input).unwrap_err().to_string(),
            expected,
            "{defect}"
        );
        assert!(input.advance(1).is_err());
        drop(input);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(
            catalog.load_tables().unwrap()[0].columns_json,
            row.columns_json
        );
    }
}

#[test]
fn retained_constraint_catalog_retires_each_cancelled_denied_and_abandoned_prefix() {
    let catalog = fixture(1);
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = ConstraintCatalogInput::new(Arc::clone(&catalog), LIMITS, &control).unwrap();
    let steps = finish(&mut input).unwrap();
    let peak = control.memory().peak();
    drop(input);
    for prefix in 0..steps - 1 {
        for terminal in ["cancel", "zero", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(1 << 20);
            let mut input =
                ConstraintCatalogInput::new(Arc::clone(&catalog), LIMITS, &control).unwrap();
            for _ in 0..prefix {
                assert!(input.advance(1).unwrap().is_none());
            }
            match terminal {
                "cancel" => {
                    control.cancellation().cancel();
                    assert!(input.advance(1).is_err());
                    assert!(input.advance(1).is_err());
                }
                "zero" => {
                    assert!(input.advance(0).is_err());
                    assert!(input.advance(1).is_err());
                }
                "drop" => (),
                "unwind" => {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _input = input;
                        panic!("retire constraint input");
                    }));
                    assert_eq!(control.memory().used(), 0);
                    continue;
                }
                _ => unreachable!(),
            }
            drop(input);
            assert_eq!(control.memory().used(), 0, "{terminal}: {prefix}");
        }
    }
    let mut denied = 0;
    let mut admitted = 0;
    // Exact growth can succeed below the peak measured with preferred buffer capacities.
    for limit in (0..peak).step_by(127).chain([peak - 1, peak]) {
        let control = StorageReadControl::with_limit(limit);
        match ConstraintCatalogInput::new(Arc::clone(&catalog), LIMITS, &control) {
            Ok(mut input) => {
                match finish(&mut input) {
                    Ok(_) => admitted += 1,
                    Err(error) => {
                        assert!(error.to_string().contains("memory"), "{error}");
                        denied += 1;
                    }
                }
                assert!(input.advance(1).is_err());
            }
            Err(error) => {
                assert!(matches!(error, StorageBackendError::Memory(_)));
                denied += 1;
            }
        }
        assert!(control.memory().peak() <= limit);
        assert_eq!(control.memory().used(), 0);
    }
    assert!(denied > 0 && admitted > 0);
}

#[test]
fn retained_constraint_catalog_finishes_physical_acl_input_before_semantic_rows() {
    let catalog = fixture(1);
    let mut row = catalog.load_tables().unwrap().remove(0);
    row.columns_json = "invalid columns".into();
    catalog.save_table(&row).unwrap();
    let key = uqa_storage::catalog::relation_acl::key(&row.relation, None);
    catalog.set_metadata(&key, "invalid ACL").unwrap();
    let expected = catalog.load_tables().unwrap_err().to_string();
    assert_eq!(
        validate_constraint_catalog(catalog.as_ref())
            .unwrap_err()
            .to_string(),
        expected
    );
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = ConstraintCatalogInput::new(Arc::clone(&catalog), LIMITS, &control).unwrap();
    let error = finish(&mut input).unwrap_err();
    assert!(matches!(error, StorageBackendError::Serde(_)), "{error}");
    assert!(!error.to_string().contains("columns"));
    assert!(input.advance(1).is_err());
    drop(input);
    assert_eq!(control.memory().used(), 0);
}
