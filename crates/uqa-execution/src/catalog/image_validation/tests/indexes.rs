//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{catalog_identity::CatalogObjectIdentity, catalog_index::CatalogIndexRow};
use uqa_sql::{
    ast::RelationPersistence,
    catalog::index::{IndexCatalogIdentity, IndexDefinition},
};

#[test]
fn native_catalog_indexes_join_original_table_incarnations_and_release_rejected_workspace() {
    let mut snapshot = snapshot();
    let table = RelationIdentity::new("public", "items");
    let row = CatalogIndexRow {
        relation: RelationIdentity::new("public", "saved"),
        table_name: table.qualified_name(),
        index_type: "btree".into(),
        columns_json: r#"["value"]"#.into(),
        parameters_json: "{}".into(),
        definition_json: Some(
            serde_json::to_string(&IndexDefinition {
                catalog: Some(IndexCatalogIdentity {
                    identity: CatalogObjectIdentity {
                        object_id: [2; 16],
                        oid: 20002,
                    },
                    table_object_id: [1; 16],
                    physical_key: "original".into(),
                }),
                ..IndexDefinition::default()
            })
            .unwrap(),
        ),
    };
    RetainedValue::make_mut(&mut snapshot.definitions.catalog_indexes)
        .insert(row, RelationPersistence::Permanent)
        .unwrap();
    let memory = MemoryBudget::new(64 << 10);
    let cancellation = CancellationToken::new();
    assert!(validate_indexes(&snapshot, &memory, &cancellation).is_err());
    assert_eq!(memory.used(), 0);
    snapshot.tables.insert(
        table.clone(),
        table_snapshot([1; 16], vec![], TableConstraintSet::default()),
    );
    validate_indexes(&snapshot, &memory, &cancellation).unwrap();
    assert_eq!(memory.used(), 0);
    snapshot.tables.get_mut(&table).unwrap().object_id = [3; 16];
    assert!(validate_indexes(&snapshot, &memory, &cancellation).is_err());
    assert_eq!(memory.used(), 0);
    assert_eq!(snapshot.tables.get(&table).unwrap().object_id, [3; 16]);
    assert_eq!(snapshot.definitions.catalog_indexes.len(), 1);
}
