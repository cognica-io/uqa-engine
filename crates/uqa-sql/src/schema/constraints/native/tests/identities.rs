//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn native_constraint_catalog_rejects_cross_table_oid_and_incarnation_collisions() {
    for repeated_object in [false, true] {
        let mut tables = vec![table("first", 1), table("second", 2)];
        for (id, table) in [10, 11].into_iter().zip(&mut tables) {
            table.columns[0].not_null = true;
            table.columns[0].not_null_identity = Some(identity(id));
        }
        accepts(&tables, &[], true);
        let duplicate = tables[1].columns[0].not_null_identity.as_mut().unwrap();
        if repeated_object {
            duplicate.object_id = identity(10).object_id;
        } else {
            duplicate.oid = identity(10).oid;
        }
        accepts(&tables, &[], false);
    }
}

#[test]
fn native_constraint_catalog_rejects_invalid_supplied_row_identities() {
    for invalid in [
        CatalogObjectIdentity {
            object_id: [0; 16],
            oid: 20_000,
        },
        CatalogObjectIdentity {
            object_id: [3; 16],
            oid: 0,
        },
        CatalogObjectIdentity {
            object_id: [3; 16],
            oid: -1,
        },
        CatalogObjectIdentity {
            object_id: [3; 16],
            oid: i64::from(u32::MAX) + 1,
        },
    ] {
        let mut tables = vec![table("source", 1)];
        tables[0].columns[0].not_null = true;
        tables[0].columns[0].not_null_identity = Some(invalid);
        accepts(&tables, &[], false);
    }
}

#[test]
fn native_constraint_catalog_includes_each_constraint_kind() {
    for kind in [
        "not_null",
        "column_check",
        "table_check",
        "key",
        "column_foreign_key",
    ] {
        let (mut tables, indexes) = fixture();
        let duplicate = identity(4);
        match kind {
            "not_null" => {
                tables[0].columns[0].not_null = true;
                tables[0].columns[0].not_null_identity = Some(duplicate);
            }
            "column_check" => {
                tables[0].columns[0].check = Some(Expr::Literal(uqa_core::Value::Bool(true)));
                tables[0].columns[0].check_object_id = Some(duplicate.object_id);
                tables[0].columns[0].check_catalog_oid = Some(duplicate.oid);
            }
            "table_check" => tables[0].checks.push(TableCheck {
                name: Some("positive".into()),
                object_id: Some(duplicate.object_id),
                catalog_oid: Some(duplicate.oid),
                is_local: true,
                expr: Expr::Literal(uqa_core::Value::Bool(true)),
                enforced: true,
                validated: false,
                no_inherit: false,
                partition_constraint: None,
            }),
            "key" => {
                let mut key: TableKeyConstraint =
                    serde_json::from_str(r#"{"kind":"Unique","columns":["id"]}"#).unwrap();
                key.catalog_identity = Some(duplicate);
                tables[0].keys.push(key);
            }
            "column_foreign_key" => {
                let mut key: ForeignKeyRef =
                    serde_json::from_str(r#"{"table":"public.parent","column":"id"}"#).unwrap();
                key.referenced_index = Some([3; 16]);
                key.catalog_identity = Some(duplicate);
                key.object_id = Some([9; 16]);
                tables[1].columns[0].references = Some(key);
            }
            _ => unreachable!(),
        }
        accepts(&tables, &indexes, false);
    }
}

#[test]
fn native_constraint_catalog_preserves_shared_families_and_separate_catalog_classes() {
    let (mut tables, indexes) = fixture();
    tables[1].hierarchy.as_mut().unwrap().partition_spec = Some(PartitionSpec {
        strategy: PartitionStrategy::Range,
        keys: vec![Expr::Column("id".into())],
    });
    let mut child = table("partition", 6);
    let mut key = tables[1].foreign_keys[0].clone();
    key.catalog_identity = Some(identity(7));
    child.foreign_keys.push(key.clone());
    let hierarchy = child.hierarchy.as_mut().unwrap();
    hierarchy.parents.push("public.child".into());
    hierarchy.partition_bound = Some(PartitionBound::Default);
    hierarchy.partition_inherited_foreign_keys.push(key);
    tables.push(child);
    tables[0].columns[0].not_null = true;
    tables[0].columns[0].not_null_identity = Some(CatalogObjectIdentity {
        object_id: [8; 16],
        // pg_class and pg_constraint have independent OID namespaces.
        oid: identity(3).oid,
    });
    accepts(&tables, &indexes, true);
    assert_eq!(
        tables[1].foreign_keys[0].object_id,
        tables[2].foreign_keys[0].object_id
    );
    tables[2].foreign_keys[0].catalog_identity = Some(identity(4));
    tables[2]
        .hierarchy
        .as_mut()
        .unwrap()
        .partition_inherited_foreign_keys[0]
        .catalog_identity = Some(identity(4));
    accepts(&tables, &indexes, false);
}

#[test]
fn native_constraint_catalog_borrows_large_expressions_and_preserves_legacy_absence() {
    let mut tables = vec![table("source", 1)];
    let column = &mut tables[0].columns[0];
    column.name = "x".repeat(1 << 20);
    column.ty = ColumnType::Boolean;
    column.check = Some(Expr::Column(column.name.clone().into()));
    column.check_object_id = Some([9; 16]);
    column.check_catalog_oid = None;
    column.check_validated = false;
    column.check_enforced = false;
    tables[0].columns[1].not_null = true;
    tables[0].columns[1].not_null_identity = None;
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let allocations = allocation_counter::measure(|| check(&tables, &[], &memory, &token).unwrap());
    assert!(allocations.bytes_total < 16 << 10, "{allocations:?}");
    assert_eq!(memory.used(), 0);
    assert_eq!(tables[0].columns[0].check_object_id, Some([9; 16]));
    assert_eq!(tables[0].columns[0].check_catalog_oid, None);
    assert!(!tables[0].columns[0].check_validated);
    assert!(!tables[0].columns[0].check_enforced);
    assert_eq!(tables[0].columns[1].not_null_identity, None);
}
