//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ast::{PartitionBound, TableConstraintSet};

fn orphan(name: &str) -> TableSchema {
    let mut schema = table(name);
    let uqa_sql::Statement::CreateTable(declaration) = uqa_sql::compile("CREATE TABLE c(v integer NOT NULL, CONSTRAINT uk UNIQUE(v), CONSTRAINT fk FOREIGN KEY(v) REFERENCES referenced(v))").unwrap().remove(0) else { panic!("table declaration") };
    let mut constraints = TableConstraintSet {
        key_constraints: declaration.key_constraints,
        foreign_keys: declaration.foreign_keys,
        ..Default::default()
    };
    constraints.hierarchy.parents = vec!["missing".into()];
    constraints.hierarchy.partition_bound = Some(PartitionBound::Default);
    constraints.foreign_keys[0].object_id = Some([42; 16]);
    constraints.hierarchy.partition_inherited_foreign_keys = constraints.foreign_keys.clone();
    constraints.hierarchy.partition_inherited_key_constraints = constraints.key_constraints.clone();
    schema.columns_json = serde_json::to_string(&declaration.columns).unwrap();
    schema.constraints_json = serde_json::to_string(&constraints).unwrap();
    schema
}

#[test]
fn restored_partition_roots_get_separate_families_while_descendants_keep_their_parent_family() {
    let catalog = catalog();
    for name in ["first", "second"] {
        catalog.save_table(&orphan(name)).unwrap();
    }
    let mut child = orphan("child");
    let mut constraints: TableConstraintSet =
        serde_json::from_str(&child.constraints_json).unwrap();
    constraints.hierarchy.parents = vec!["first".into()];
    child.constraints_json = serde_json::to_string(&constraints).unwrap();
    catalog.save_table(&child).unwrap();
    migrate_constraint_catalog(&catalog).unwrap();
    validate_constraint_catalog(&catalog).unwrap();
    let restored: BTreeMap<_, TableConstraintSet> = catalog
        .load_tables()
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.relation.name,
                serde_json::from_str(&row.constraints_json).unwrap(),
            )
        })
        .collect();
    let first = &restored["first"];
    let second = &restored["second"];
    let child = &restored["child"];
    assert!(!first.hierarchy.is_partition());
    assert_eq!(first.key_constraints.len(), 1);
    assert_eq!(first.foreign_keys.len(), 1);
    assert_ne!(
        first.foreign_keys[0].object_id,
        second.foreign_keys[0].object_id
    );
    assert_eq!(
        child.foreign_keys[0].object_id,
        first.foreign_keys[0].object_id
    );
    assert_eq!(child.hierarchy.parents, ["public.first"]);
    assert_eq!(
        child.hierarchy.partition_inherited_foreign_keys,
        child.foreign_keys
    );
    let before: BTreeMap<_, _> = catalog
        .load_tables()
        .unwrap()
        .into_iter()
        .map(|row| (row.relation, (row.columns_json, row.constraints_json)))
        .collect();
    migrate_constraint_catalog(&catalog).unwrap();
    let after: BTreeMap<_, _> = catalog
        .load_tables()
        .unwrap()
        .into_iter()
        .map(|row| (row.relation, (row.columns_json, row.constraints_json)))
        .collect();
    assert_eq!(after, before);
}

#[test]
fn later_foreign_metadata_failure_does_not_publish_parent_repairs() {
    let catalog = catalog();
    let original = orphan("first");
    catalog.save_table(&original).unwrap();
    catalog
        .save_foreign_table(&uqa_storage::ForeignTableRow {
            relation: RelationIdentity::new("public", "invalid"),
            security: RelationSecurityRow::legacy("uqa"),
            server_name: "memory".into(),
            columns_json: "{".into(),
            options_json: "{}".into(),
        })
        .unwrap();
    assert!(migrate_constraint_catalog(&catalog).is_err());
    let retained = catalog.load_tables().unwrap().remove(0);
    assert_eq!(retained.constraints_json, original.constraints_json);
    assert_eq!(retained.columns_json, original.columns_json);
    assert!(catalog
        .get_metadata(CATALOG_ADDRESS_METADATA_KEY)
        .unwrap()
        .is_none());
}
