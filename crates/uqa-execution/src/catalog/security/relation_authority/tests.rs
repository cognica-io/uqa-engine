//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private view records take precedence without reverting another transaction's definitions.

use super::*;
use std::collections::BTreeSet;
use uqa_core::Value;
use uqa_sql::{catalog::stored_view::StoredViewDefinition, plan::UnifiedPlan};

fn view(identity: u8) -> StoredView {
    let UnifiedPlan::Query(query) =
        UnifiedPlan::lower(uqa_sql::compile("SELECT 7 AS v").unwrap().remove(0))
    else {
        unreachable!()
    };
    StoredView {
        security: BoundTableSecurity::owner(uqa_sql::catalog::roles::RoleIdentity {
            oid: 16_384 + i64::from(identity),
            object_id: [identity; 16],
        }),
        definition: StoredViewDefinition {
            object_id: [identity; 16],
            query: *query,
            output_columns: Some(vec!["v".into()]),
            persistence: uqa_sql::ast::RelationPersistence::Permanent,
            options: vec![],
            kind: uqa_sql::catalog::view::StoredViewKind::Materialized,
            materialized_rows: vec![[("v".into(), Value::Int(7))].into()],
            materialized_column_types: vec![Some(uqa_sql::ast::ColumnType::Integer)],
            materialized_column_collations: Some(vec![0]),
            populated: true,
            catalog_oids: None,
            row_type_array_name: Some("_saved".into()),
        },
    }
}

fn relation(name: &str) -> RelationIdentity {
    RelationIdentity::new("public", name)
}

#[test]
fn private_view_records_preserve_complete_definitions_and_peer_changes() {
    let latest = Arc::new(BTreeMap::from([
        (relation("dropped"), view(1)),
        (relation("renamed_from"), view(2)),
        (relation("replaced"), view(3)),
        (relation("peer"), view(4)),
        (relation("recreated"), view(5)),
    ]));
    let mut replacement = view(3);
    replacement.materialized_rows.clear();
    replacement.populated = false;
    let current = BTreeMap::from([
        (relation("created"), view(6)),
        (relation("renamed_to"), view(2)),
        (relation("replaced"), replacement),
        (relation("peer"), view(7)),
        (relation("recreated"), view(8)),
    ]);
    let mut visited = BTreeSet::new();
    let merged = merge_private_view_definitions(&current, latest.clone(), |name| {
        assert!(
            visited.insert(name.clone()),
            "one provenance read per exact key"
        );
        Ok(name.name != "peer")
    })
    .unwrap();
    assert_eq!(visited.len(), 7);
    assert_eq!(merged.len(), 5);
    for name in ["created", "renamed_to", "replaced", "recreated"] {
        let name = relation(name);
        assert_eq!(
            serde_json::to_value(&merged[&name]).unwrap(),
            serde_json::to_value(&current[&name]).unwrap(),
        );
        assert_eq!(merged[&name].security, current[&name].security);
    }
    assert_eq!(merged[&relation("peer")].object_id, [4; 16]);
    assert!(!merged.contains_key(&relation("dropped")));
    assert!(!merged.contains_key(&relation("renamed_from")));
    assert!(latest[&relation("replaced")].populated);
    assert_eq!(latest[&relation("recreated")].object_id, [5; 16]);
}

#[test]
fn unchanged_views_share_the_committed_map_and_provenance_errors_do_not_publish() {
    let latest = Arc::new(BTreeMap::from([(relation("saved"), view(1))]));
    let current = BTreeMap::from([(relation("saved"), view(2))]);
    let merged = merge_private_view_definitions(&current, latest.clone(), |_| Ok(false)).unwrap();
    assert!(Arc::ptr_eq(&latest, &merged));
    let error = merge_private_view_definitions(&current, latest.clone(), |_| {
        Err(uqa_storage::StorageBackendError::Other(
            "provenance unavailable".into(),
        ))
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "provenance unavailable");
    assert_eq!(latest[&relation("saved")].object_id, [1; 16]);
}
