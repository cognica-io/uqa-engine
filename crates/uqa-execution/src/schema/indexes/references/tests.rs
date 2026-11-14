//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::RelationIdentity;
use uqa_sql::{ast::RelationPersistence, catalog::index::IndexDefinition};

#[test]
fn table_rename_prepares_whole_row_bindings_without_changing_the_live_registry() {
    let uqa_sql::Statement::CreateIndex(index) = uqa_sql::compile(
        "CREATE INDEX saved ON items(row_key(items.*)) WHERE row_enabled(items.*)",
    )
    .unwrap()
    .remove(0) else {
        panic!("index declaration")
    };
    let original = CatalogIndexRow {
        relation: RelationIdentity::new("public", "saved"),
        index_type: "btree".into(),
        table_name: "public.items".into(),
        columns_json: serde_json::to_string(&index.columns).unwrap(),
        parameters_json: "{}".into(),
        definition_json: Some(
            serde_json::to_string(&IndexDefinition {
                predicate: index.predicate,
                ..Default::default()
            })
            .unwrap(),
        ),
    };
    let mut rows = CatalogIndexes::default();
    rows.insert(original.clone(), RelationPersistence::Permanent)
        .unwrap();
    let prepared = table_rename(&rows, "public.items", "public.renamed").unwrap();
    assert_eq!(
        format!("{:?}", rows.get(&original.relation).unwrap()),
        format!("{original:?}")
    );
    assert_eq!(prepared.updates.len(), 1);
    let updated = &prepared.updates[0];
    assert_eq!(updated.table_name, "public.renamed");
    assert_eq!(updated.relation, original.relation);
    assert_eq!(updated.parameters_json, original.parameters_json);
    let uqa_sql::Statement::CreateIndex(expected) = uqa_sql::compile(
        "CREATE INDEX saved ON renamed(row_key(renamed.*)) WHERE row_enabled(renamed.*)",
    )
    .unwrap()
    .remove(0) else {
        panic!("index declaration")
    };
    assert_eq!(
        serde_json::from_str::<Vec<IndexKey>>(&updated.columns_json).unwrap(),
        expected.columns
    );
    assert_eq!(
        index_definition(updated).unwrap().predicate,
        expected.predicate
    );
    assert_eq!(
        format!("{:?}", prepared.rows.get(&original.relation).unwrap()),
        format!("{updated:?}")
    );
}
