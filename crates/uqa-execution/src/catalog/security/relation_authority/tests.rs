//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private view records take precedence without reverting another transaction's definitions.

use super::*;
use crate::catalog::foreign::StoredForeignTable;
use std::collections::BTreeSet;
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::{
    catalog::stored_view::{StoredView, StoredViewDefinition, ViewDefinitions},
    plan::UnifiedPlan,
};

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
            materialized_rows: vec![[("v".into(), Value::Int(7))].into_iter().collect()],
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
    let latest = uqa_core::memory::RetainedValue::new(
        ViewDefinitions::try_from_iter([
            (relation("dropped"), view(1)),
            (relation("renamed_from"), view(2)),
            (relation("replaced"), view(3)),
            (relation("peer"), view(4)),
            (relation("recreated"), view(5)),
        ])
        .unwrap(),
    );
    let mut replacement = view(3);
    replacement.materialized_rows.clear();
    replacement.populated = false;
    let current = ViewDefinitions::try_from_iter([
        (relation("created"), view(6)),
        (relation("renamed_to"), view(2)),
        (relation("replaced"), replacement),
        (relation("peer"), view(7)),
        (relation("recreated"), view(8)),
    ])
    .unwrap();
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
    let latest = uqa_core::memory::RetainedValue::new(
        ViewDefinitions::try_from_iter([(relation("saved"), view(1))]).unwrap(),
    );
    let current = ViewDefinitions::try_from_iter([(relation("saved"), view(2))]).unwrap();
    let merged = merge_private_view_definitions(&current, latest.clone(), |_| Ok(false)).unwrap();
    assert!(uqa_core::memory::RetainedValue::ptr_eq(&latest, &merged));
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
        options: [("source".into(), identity.to_string())]
            .into_iter()
            .collect(),
        option_order: vec!["source".into()],
    }
}

fn foreign_catalog(
    values: impl IntoIterator<Item = (RelationIdentity, StoredForeignTable)>,
) -> ForeignCatalog {
    let mut result = ForeignCatalog::default();
    for (name, table) in values {
        let security = view(table.object_id[0]).security;
        result.insert(name, table, security).unwrap();
    }
    result
}

#[test]
fn private_foreign_records_preserve_definitions_security_and_peer_changes() {
    let latest = foreign_catalog([
        (relation("dropped"), foreign_table(1)),
        (relation("renamed_from"), foreign_table(2)),
        (relation("replaced"), foreign_table(3)),
        (relation("peer"), foreign_table(4)),
        (relation("recreated"), foreign_table(5)),
    ]);
    let mut replacement = foreign_table(3);
    replacement
        .options
        .insert("source".into(), "changed".into());
    let mut current = foreign_catalog([
        (relation("created"), foreign_table(6)),
        (relation("renamed_to"), foreign_table(2)),
        (relation("replaced"), replacement),
        (relation("peer"), foreign_table(7)),
        (relation("recreated"), foreign_table(8)),
    ]);
    current
        .replace_security(&relation("replaced"), view(9).security)
        .unwrap();
    let mut merged = latest.clone();
    let mut visited = BTreeSet::new();
    merge_private_foreign_definitions(&current, &mut merged, |name| {
        assert!(
            visited.insert(name.clone()),
            "one provenance read per exact key"
        );
        Ok(name.name != "peer")
    })
    .unwrap();
    assert_eq!(visited.len(), 7);
    assert_eq!(merged.tables().len(), 5);
    assert!(merged
        .tables()
        .keys()
        .eq(merged.security().iter().map(|(key, _)| key)));
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
        assert_eq!(
            definition(&merged.tables()[&name]),
            definition(&current.tables()[&name])
        );
        assert_eq!(merged.security()[&name], current.security()[&name]);
    }
    assert_eq!(merged.tables()[&relation("peer")].object_id, [4; 16]);
    assert_eq!(
        merged.security()[&relation("peer")],
        latest.security()[&relation("peer")]
    );
    assert!(!merged.tables().contains_key(&relation("dropped")));
    assert!(!merged.tables().contains_key(&relation("renamed_from")));
    assert_eq!(latest.tables()[&relation("recreated")].object_id, [5; 16]);
}

#[test]
fn unchanged_foreign_generations_share_storage_and_failed_provenance_keeps_both_roots() {
    let name = relation("saved");
    let original = foreign_catalog([(name.clone(), foreign_table(1))]);
    let current = foreign_catalog([(name.clone(), foreign_table(2))]);
    let mut latest = original.clone();
    merge_private_foreign_definitions(&current, &mut latest, |_| Ok(false)).unwrap();
    assert!(Arc::ptr_eq(
        &latest.tables_snapshot(),
        &original.tables_snapshot()
    ));
    assert!(Arc::ptr_eq(
        &latest.security_snapshot(),
        &original.security_snapshot()
    ));
    let error = merge_private_foreign_definitions(&current, &mut latest, |_| {
        Err(StorageBackendError::Other("provenance unavailable".into()))
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "provenance unavailable");
    assert!(Arc::ptr_eq(
        &latest.tables_snapshot(),
        &original.tables_snapshot()
    ));
    assert!(Arc::ptr_eq(
        &latest.security_snapshot(),
        &original.security_snapshot()
    ));
}
