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

fn foreign_table(identity: u8) -> StoredForeignTable {
    StoredForeignTable {
        name: "public.saved".into(),
        persistence: uqa_sql::ast::RelationPersistence::Permanent,
        object_id: [identity; 16],
        catalog_oids: None,
        row_type_array_name: Some("_saved".into()),
        server_name: "remote".into(),
        server_reference: Some(crate::catalog::foreign::ForeignServerReference {
            oid: 70_000,
            object_id: [20; 16],
        }),
        columns: Vec::new(),
        dropped_attributes: Vec::new(),
        checks: Vec::new(),
        options: BTreeMap::from([("source".into(), identity.to_string())]),
        option_order: vec!["source".into()],
    }
}

#[test]
fn private_foreign_records_preserve_definitions_security_and_peer_changes() {
    let latest = Arc::new(BTreeMap::from([
        (relation("dropped"), foreign_table(1)),
        (relation("renamed_from"), foreign_table(2)),
        (relation("replaced"), foreign_table(3)),
        (relation("peer"), foreign_table(4)),
        (relation("recreated"), foreign_table(5)),
    ]));
    let mut replacement = foreign_table(3);
    replacement
        .options
        .insert("source".into(), "changed".into());
    let current = BTreeMap::from([
        (relation("created"), foreign_table(6)),
        (relation("renamed_to"), foreign_table(2)),
        (relation("replaced"), replacement),
        (relation("peer"), foreign_table(7)),
        (relation("recreated"), foreign_table(8)),
    ]);
    let security = |tables: &BTreeMap<RelationIdentity, StoredForeignTable>| {
        tables
            .iter()
            .map(|(name, table)| (name.clone(), view(table.object_id[0]).security))
            .collect::<BTreeMap<_, _>>()
    };
    let mut current_security = security(&current);
    current_security.insert(relation("replaced"), view(9).security);
    let latest_security = Arc::new(security(&latest));
    let mut merged = latest.clone();
    let mut merged_security = latest_security.clone();
    let mut visited = BTreeSet::new();
    merge_private_foreign_definitions(
        &current,
        &current_security,
        &mut merged,
        &mut merged_security,
        |name| {
            assert!(
                visited.insert(name.clone()),
                "one provenance read per exact key"
            );
            Ok(name.name != "peer")
        },
    )
    .unwrap();
    assert_eq!(visited.len(), 7);
    assert_eq!(merged.len(), 5);
    assert_eq!(
        merged.keys().collect::<Vec<_>>(),
        merged_security.keys().collect::<Vec<_>>()
    );
    let definition = |table: &StoredForeignTable| {
        (
            table.name.clone(),
            table.persistence,
            table.server_name.clone(),
            table.options.clone(),
            table.schema_json().unwrap(),
        )
    };
    for name in ["created", "renamed_to", "replaced", "recreated"] {
        let name = relation(name);
        assert_eq!(definition(&merged[&name]), definition(&current[&name]));
        assert_eq!(merged_security[&name], current_security[&name]);
    }
    assert_eq!(merged[&relation("peer")].object_id, [4; 16]);
    assert_eq!(
        merged_security[&relation("peer")],
        latest_security[&relation("peer")]
    );
    assert!(!merged.contains_key(&relation("dropped")));
    assert!(!merged.contains_key(&relation("renamed_from")));
    assert_eq!(latest[&relation("recreated")].object_id, [5; 16]);
}

#[test]
fn unchanged_foreign_maps_share_storage_and_invalid_private_security_is_rejected() {
    let name = relation("saved");
    let original = Arc::new(BTreeMap::from([(name.clone(), foreign_table(1))]));
    let original_security = Arc::new(BTreeMap::from([(name.clone(), view(1).security)]));
    let current = BTreeMap::from([(name.clone(), foreign_table(2))]);
    let mut latest = original.clone();
    let mut security = original_security.clone();
    merge_private_foreign_definitions(
        &current,
        &BTreeMap::new(),
        &mut latest,
        &mut security,
        |_| Ok(false),
    )
    .unwrap();
    assert!(Arc::ptr_eq(&latest, &original));
    assert!(Arc::ptr_eq(&security, &original_security));
    let error = merge_private_foreign_definitions(
        &current,
        &BTreeMap::new(),
        &mut latest,
        &mut security,
        |_| Ok(true),
    )
    .unwrap_err();
    assert!(error.to_string().contains("has no security metadata"));
    assert!(Arc::ptr_eq(&latest, &original));
    assert!(Arc::ptr_eq(&security, &original_security));
    let error = merge_private_foreign_definitions(
        &current,
        &BTreeMap::new(),
        &mut latest,
        &mut security,
        |_| {
            Err(uqa_storage::StorageBackendError::Other(
                "provenance unavailable".into(),
            ))
        },
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "provenance unavailable");
    assert!(Arc::ptr_eq(&latest, &original));
    assert!(Arc::ptr_eq(&security, &original_security));
}
