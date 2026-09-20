//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn fixture() -> (TableSchema, TableConstraintSet, KeyConstraintNames) {
    let key: TableKeyConstraint = serde_json::from_value(serde_json::json!({
        "name": "original", "columns": ["v"], "kind": "Unique",
        "catalog_identity": {"object_id": vec![3; 16], "oid": 16384}
    }))
    .unwrap();
    let constraints = TableConstraintSet {
        key_constraints: vec![key],
        ..Default::default()
    };
    let schema = TableSchema {
        relation: RelationIdentity::new("public", "t"),
        security: uqa_storage::RelationSecurityRow::legacy("uqa"),
        object_id: [1; 16],
        storage_generation: [2; 16],
        analyzer_json: "{}".into(),
        fts_fields: Vec::new(),
        vector_fields: Vec::new(),
        columns_json: "[]".into(),
        constraints_json: encode(&constraints).unwrap(),
    };
    let names = KeyConstraintNames {
        names: Some(BTreeMap::from([(
            [3; 16],
            KeyName {
                table: schema.relation.clone(),
                table_object_id: schema.object_id,
                name: "current".into(),
            },
        )])),
    };
    (schema, constraints, names)
}

#[test]
fn structural_storage_rehydrates_the_current_owned_name_without_changing_identity() {
    let (schema, mut expected, names) = fixture();
    let stored: TableConstraintSet = serde_json::from_str(&schema.constraints_json).unwrap();
    assert!(stored.key_constraints[0].name.is_none());
    expected.key_constraints[0].name = Some("current".into());
    assert_eq!(
        serde_json::to_value(names.decode(&schema).unwrap()).unwrap(),
        serde_json::to_value(expected).unwrap(),
    );
}

#[test]
fn missing_recreated_or_redundant_name_owners_are_rejected() {
    let (schema, constraints, mut names) = fixture();
    let mut recreated = schema.clone();
    recreated.object_id = [4; 16];
    assert!(names.decode(&recreated).is_err());
    let mut duplicated = schema.clone();
    duplicated.constraints_json = serde_json::to_string(&constraints).unwrap();
    assert!(names.decode(&duplicated).is_err());
    names.names.as_mut().unwrap().clear();
    assert!(names.decode(&schema).is_err());
}

#[test]
fn a_legacy_declaration_keeps_its_name_until_registry_conversion() {
    let (mut schema, constraints, _) = fixture();
    let names = KeyConstraintNames { names: None };
    schema.constraints_json = names.encode(&constraints).unwrap();
    assert_eq!(
        serde_json::to_value(names.decode(&schema).unwrap()).unwrap(),
        serde_json::to_value(constraints).unwrap(),
    );
}
