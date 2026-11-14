//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn declaration(sql: &str) -> CreateIndex {
    let crate::Statement::CreateIndex(index) = crate::compile(sql).unwrap().remove(0) else {
        panic!("index declaration")
    };
    index
}

#[test]
fn renamed_stored_index_preserves_identity_display_names_and_sql_references() {
    let original =
        declaration("CREATE INDEX i ON items(old, (old + 1)) INCLUDE (kept) WHERE old > 0");
    let expected =
        declaration("CREATE INDEX i ON items(new, (new + 1)) INCLUDE (kept) WHERE new > 0");
    let mut keys = original.columns;
    let mut definition = IndexDefinition {
        catalog: Some(super::super::IndexCatalogIdentity {
            identity: uqa_core::catalog_identity::CatalogObjectIdentity {
                object_id: [7; 16],
                oid: 42_000,
            },
            table_object_id: [8; 16],
            physical_key: "original physical key".into(),
        }),
        predicate: original.predicate,
        included_columns: original.included_columns,
        ..Default::default()
    };
    let identity = definition.catalog.clone();
    rename_column_references(&mut keys, &mut definition, "old", "new").unwrap();
    assert_eq!(keys, expected.columns);
    assert_eq!(definition.predicate, expected.predicate);
    assert_eq!(definition.included_columns, expected.included_columns);
    assert_eq!(definition.key_names, ["old", "kept"]);
    assert_eq!(definition.catalog, identity);
    rename_column_references(&mut keys, &mut definition, "kept", "renamed_kept").unwrap();
    assert_eq!(definition.included_columns, ["renamed_kept"]);
    assert_eq!(definition.key_names, ["old", "kept"]);
    assert_eq!(definition.catalog, identity);
}

#[test]
fn renamed_relation_preserves_whole_row_scope_without_rebinding_column_names() {
    let original = declaration(
        "CREATE INDEX i ON items(items, row_key(items.*)) INCLUDE(id) WHERE row_enabled(items.*)",
    );
    let expected = declaration(
        "CREATE INDEX i ON renamed(items, row_key(renamed.*)) INCLUDE(id) WHERE row_enabled(renamed.*)",
    );
    let mut keys = original.columns;
    let mut definition = IndexDefinition {
        predicate: original.predicate,
        included_columns: original.included_columns,
        key_names: vec!["items".into(), "row_key".into(), "id".into()],
        ..Default::default()
    };
    rename_relation_references(
        &mut keys,
        &mut definition,
        &uqa_core::RelationIdentity::new("public", "items"),
        "public.renamed",
    )
    .unwrap();
    assert_eq!(keys, expected.columns);
    assert_eq!(definition.predicate, expected.predicate);
    assert_eq!(definition.included_columns, expected.included_columns);
    assert_eq!(definition.key_names, ["items", "row_key", "id"]);
}
