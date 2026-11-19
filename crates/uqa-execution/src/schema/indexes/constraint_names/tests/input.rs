//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::catalog::index::{IndexCatalogIdentity, IndexDefinition, IndexRelationships};
use uqa_storage::CatalogIndexRow;

mod catalog;
mod native;
mod retention;

fn row() -> CatalogIndexRow {
    CatalogIndexRow {
        relation: RelationIdentity::new("public", "current"),
        index_type: "btree".into(),
        table_name: "public.t".into(),
        columns_json: r#"["v"]"#.into(),
        parameters_json: "{}".into(),
        definition_json: Some(
            serde_json::to_string(&IndexDefinition {
                catalog: Some(IndexCatalogIdentity {
                    identity: uqa_core::catalog_identity::CatalogObjectIdentity {
                        object_id: [7; 16],
                        oid: 40007,
                    },
                    table_object_id: [1; 16],
                    physical_key: "physical".into(),
                }),
                relationships: IndexRelationships {
                    owning_constraint: Some([3; 16]),
                    parent_index: None,
                },
                unique: true,
                ..IndexDefinition::default()
            })
            .unwrap(),
        ),
    }
}

fn names(row: &CatalogIndexRow, memory: &MemoryBudget) -> KeyConstraintNames {
    let token = CancellationToken::new();
    let mut input = KeyConstraintNameInput::new(Some("2"), memory, &token, &token).unwrap();
    input.push(row).unwrap();
    input.finish().unwrap()
}

#[test]
fn admitted_key_names_preserve_current_names_and_inherited_structure() {
    let (mut schema, mut expected, _) = fixture();
    expected.hierarchy.partition_inherited_key_constraints = expected.key_constraints.clone();
    schema.constraints_json = encode(&expected).unwrap();
    let row = row();
    let catalog =
        uqa_storage::KeyValueCatalog::new(Arc::new(uqa_storage::MemoryKeyValueStore::new()));
    catalog
        .save_schema_row(&uqa_storage::SchemaRow::bootstrap("public"))
        .unwrap();
    catalog.set_metadata(REGISTRY_VERSION, "2").unwrap();
    catalog.save_table(&schema).unwrap();
    catalog.save_catalog_index_row(&row).unwrap();
    let ordinary = KeyConstraintNames::load(&catalog)
        .unwrap()
        .decode(&schema)
        .unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let names = names(&row, &memory);
    let token = CancellationToken::new();
    let restored = names
        .decode_with_control(&schema, &memory, &token, &token)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&*restored).unwrap(),
        serde_json::to_value(ordinary).unwrap()
    );
    assert_eq!(restored.key_constraints[0].name.as_deref(), Some("current"));
    assert_eq!(
        restored.hierarchy.partition_inherited_key_constraints[0]
            .name
            .as_deref(),
        Some("current")
    );
    token.cancel();
    drop(names);
    assert_eq!(memory.used(), restored.reserved_bytes());
    assert_eq!(restored.key_constraints[0].columns, ["v"]);
    drop(restored);
    assert_eq!(memory.used(), 0);
}

#[test]
fn admitted_key_names_preserve_legacy_versions_and_reject_unknown_formats() {
    let (mut schema, expected, _) = fixture();
    schema.constraints_json = serde_json::to_string(&expected).unwrap();
    let mut invalid = row();
    invalid.definition_json = Some("}".into());
    for version in [None, Some("1")] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = KeyConstraintNameInput::new(version, &memory, &token, &token).unwrap();
        input.push(&invalid).unwrap();
        let names = input.finish().unwrap();
        let restored = names
            .decode_with_control(&schema, &memory, &token, &token)
            .unwrap();
        assert_eq!(
            serde_json::to_value(&*restored).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        drop((names, restored));
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    for version in ["", "0", "3", "2 "] {
        assert!(KeyConstraintNameInput::new(Some(version), &memory, &token, &token).is_err());
    }
    assert_eq!(memory.peak(), 0);
}

#[test]
fn admitted_key_name_input_retires_every_prefix_after_invalid_ownership_or_definition() {
    for defect in [
        "duplicate",
        "schema",
        "missing_address",
        "relation",
        "type",
        "unrelated_type",
    ] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = KeyConstraintNameInput::new(Some("2"), &memory, &token, &token).unwrap();
        input.push(&row()).unwrap();
        assert!(memory.used() > 0);
        let mut bad = row();
        match defect {
            "schema" => bad.relation.schema = "other".into(),
            "relation" => bad.table_name = "a.b.c".into(),
            "duplicate" => (),
            _ => {
                let mut definition: serde_json::Value =
                    serde_json::from_str(bad.definition_json.as_ref().unwrap()).unwrap();
                if defect == "missing_address" {
                    definition["catalog"] = serde_json::Value::Null;
                } else {
                    definition["key_types"] = serde_json::json!([null]);
                }
                if defect == "unrelated_type" {
                    definition["relationships"] = serde_json::json!({});
                }
                bad.definition_json = Some(definition.to_string());
            }
        }
        assert!(input.push(&bad).is_err(), "{defect}");
        assert_eq!(memory.used(), 0, "{defect}");
        assert!(input.push(&row()).is_err());
        assert!(input.finish().is_err());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn admitted_key_names_reject_missing_recreated_and_redundant_table_owners() {
    let (schema, _, _) = fixture();
    let memory = MemoryBudget::new(1 << 20);
    let names = names(&row(), &memory);
    let retained = memory.used();
    let token = CancellationToken::new();
    for defect in ["relation", "incarnation", "constraint", "name", "identity"] {
        let mut bad = schema.clone();
        match defect {
            "relation" => bad.relation.name = "other".into(),
            "incarnation" => bad.object_id = [8; 16],
            _ => {
                let mut constraints: TableConstraintSet =
                    serde_json::from_str(&bad.constraints_json).unwrap();
                if defect == "constraint" {
                    constraints.key_constraints[0]
                        .catalog_identity
                        .as_mut()
                        .unwrap()
                        .object_id = [9; 16];
                } else if defect == "identity" {
                    constraints.key_constraints[0].catalog_identity = None;
                } else {
                    constraints.key_constraints[0].name = Some("old".into());
                }
                bad.constraints_json = serde_json::to_string(&constraints).unwrap();
            }
        }
        assert!(
            names
                .decode_with_control(&bad, &memory, &token, &token)
                .is_err(),
            "{defect}"
        );
        assert_eq!(memory.used(), retained, "{defect}");
    }
    drop(names);
    assert_eq!(memory.used(), 0);
}
