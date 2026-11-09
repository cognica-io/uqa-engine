//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{catalog_identity::CatalogObjectIdentity, catalog_index::CatalogIndexRow};
use uqa_sql::{
    ast::{ForeignKey, RelationPersistence},
    catalog::index::{IndexCatalogIdentity, IndexDefinition},
    ColumnType,
};

#[test]
fn native_catalog_constraint_validation_joins_original_foreign_key_targets_without_mutation() {
    let mut snapshot = snapshot();
    let parent = RelationIdentity::new("public", "parent");
    let child = RelationIdentity::new("public", "child");
    let column = || ColumnDef::nullable("id", ColumnType::Integer);
    let mut key: ForeignKey = serde_json::from_str(
        r#"{"local_columns":["id"],"ref_table":"public.parent","ref_columns":["id"]}"#,
    )
    .unwrap();
    key.referenced_index = Some([3; 16]);
    snapshot.tables.insert(
        parent.clone(),
        table_snapshot([1; 16], vec![column()], TableConstraintSet::default()),
    );
    snapshot.tables.insert(
        child.clone(),
        table_snapshot(
            [2; 16],
            vec![column()],
            TableConstraintSet {
                foreign_keys: vec![key],
                ..TableConstraintSet::default()
            },
        ),
    );
    RetainedValue::make_mut(&mut snapshot.definitions.catalog_indexes)
        .insert(
            CatalogIndexRow {
                relation: RelationIdentity::new("public", "saved"),
                table_name: parent.qualified_name(),
                index_type: "btree".into(),
                columns_json: r#"["id"]"#.into(),
                parameters_json: "{}".into(),
                definition_json: Some(
                    serde_json::to_string(&IndexDefinition {
                        unique: true,
                        catalog: Some(IndexCatalogIdentity {
                            identity: CatalogObjectIdentity {
                                object_id: [3; 16],
                                oid: 20003,
                            },
                            table_object_id: [1; 16],
                            physical_key: "original".into(),
                        }),
                        ..IndexDefinition::default()
                    })
                    .unwrap(),
                ),
            },
            RelationPersistence::Permanent,
        )
        .unwrap();
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    validate_constraints(&snapshot, &memory, &token).unwrap();
    assert_eq!(memory.used(), 0);
    let original = snapshot.tables.remove(&parent).unwrap();
    assert!(validate_constraints(&snapshot, &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
    snapshot.tables.insert(parent.clone(), original);
    snapshot.tables.get_mut(&parent).unwrap().object_id = [8; 16];
    assert!(validate_constraints(&snapshot, &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
    assert_eq!(
        snapshot.tables.get(&child).unwrap().foreign_keys[0].referenced_index,
        Some([3; 16])
    );
}
