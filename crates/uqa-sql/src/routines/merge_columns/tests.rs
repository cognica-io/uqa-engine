//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct Catalog(Vec<ColumnDef>);
impl StoredMergeColumnCatalog for Catalog {
    fn stored_merge_target_definitions(&self, _: &str) -> Option<Vec<ColumnDef>> {
        Some(self.0.clone())
    }
}

#[test]
fn deleting_group_targets_keeps_original_source_positions_and_identity() {
    let Statement::CreateTable(create) =
        crate::compile("CREATE TABLE t(id integer,a integer,b integer,c integer)")
            .unwrap()
            .remove(0)
    else {
        panic!("table")
    };
    let mut catalog = Catalog(create.columns);
    for (position, column) in catalog.0.iter_mut().enumerate() {
        column.object_id = Some([u8::try_from(position).unwrap(); 16]);
    }
    let mut statement = crate::compile("MERGE INTO t USING (SELECT 1 AS id) s ON t.id=s.id WHEN MATCHED THEN UPDATE SET(a,b,c)=(SELECT 11,22,33)").unwrap().remove(0);
    bind_stored_merge_target_columns(&catalog, &mut statement).unwrap();
    let stored = serde_json::to_string(&statement).unwrap();
    catalog
        .0
        .retain(|column| column.name != "a" && column.name != "c");
    catalog
        .0
        .iter_mut()
        .find(|column| column.name == "b")
        .unwrap()
        .name = "renamed".into();
    let mut rendered: Statement = serde_json::from_str(&stored).unwrap();
    render_stored_merge_target_columns(&catalog, &mut rendered).unwrap();
    let crate::ast::Statement::Merge(merge) = &rendered else {
        panic!("merge")
    };
    let MergeWhen::UpdateMatched { assignments, .. } = &merge.when_clauses[0] else {
        panic!("update")
    };
    assert_eq!(
        assignments[0].0.column_names().collect::<Vec<_>>(),
        [
            "........pg.dropped.2........",
            "renamed",
            "........pg.dropped.4........"
        ]
    );
    normalize_stored_merge_target_columns(&catalog, &mut statement).unwrap();
    let Statement::Merge(merge) = &statement else {
        panic!("merge")
    };
    let MergeWhen::UpdateMatched { assignments, .. } = &merge.when_clauses[0] else {
        panic!("update")
    };
    let crate::ast::AssignmentTargets::Multiple(group) = &assignments[0].0 else {
        panic!("group")
    };
    assert_eq!(group.targets[0].column, "renamed");
    assert_eq!(group.source_positions, [1]);
    assert_eq!(group.source_width, 3);
    let mut restored: Statement = serde_json::from_str(&stored).unwrap();
    normalize_stored_merge_target_columns(&catalog, &mut restored).unwrap();
    assert_eq!(
        serde_json::to_value(&statement).unwrap(),
        serde_json::to_value(&restored).unwrap()
    );
}
