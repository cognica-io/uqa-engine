//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::foreign::{ForeignCatalog, StoredForeignTable};
use uqa_sql::{ast::RelationPersistence, catalog::roles::RoleIdentity};
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

fn table(name: &str, persistence: RelationPersistence) -> StoredForeignTable {
    let mut table = StoredForeignTable::from_catalog(
        name.into(),
        "source".into(),
        BTreeMap::new(),
        r#"{"version":1,"object_id":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"columns":[]}"#,
    )
    .unwrap()
    .0;
    table.persistence = persistence;
    table.server_reference = Some(crate::catalog::foreign::ForeignServerReference {
        oid: 70_000,
        object_id: [8; 16],
    });
    table
}

#[test]
fn temporary_foreign_publication_leaves_durable_definitions_unchanged() {
    let catalog = KeyValueCatalog::new(std::sync::Arc::new(MemoryKeyValueStore::new()));
    catalog
        .save_schema_row(&uqa_storage::SchemaRow::bootstrap("public"))
        .unwrap();
    let security = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
    let permanent = RelationIdentity::new("public", "durable");
    table("public.durable", RelationPersistence::Permanent)
        .persist(Some(&catalog), &permanent, &security)
        .unwrap();
    let before = catalog.load_foreign_tables().unwrap()[0]
        .columns_json
        .clone();
    let temporary = RelationIdentity::new("pg_temp_1", "private");
    let local = table("pg_temp_1.private", RelationPersistence::Temporary);
    local
        .persist(Some(&catalog), &temporary, &security)
        .unwrap();
    let rows = catalog.load_foreign_tables().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].relation, permanent);
    assert_eq!(rows[0].columns_json, before);
    let restored = StoredForeignTable::from_catalog(
        "public.durable".into(),
        "source".into(),
        BTreeMap::new(),
        &before,
    )
    .unwrap()
    .0;
    assert_eq!(restored.persistence, RelationPersistence::Permanent);
    assert!(!before.contains("persistence"));
}

#[test]
fn durable_refresh_preserves_only_temporary_foreign_definitions_and_security() {
    let mut current = ForeignCatalog::default();
    let security = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
    let temporary = RelationIdentity::new("pg_temp_1", "private");
    current
        .insert(
            temporary.clone(),
            table("pg_temp_1.private", RelationPersistence::Temporary),
            security.clone(),
        )
        .unwrap();
    let removed = RelationIdentity::new("public", "removed");
    current
        .insert(
            removed.clone(),
            table("public.removed", RelationPersistence::Permanent),
            security.clone(),
        )
        .unwrap();
    let mut restored = ForeignCatalog::default();
    merge_session_relations(&current, &mut restored).unwrap();
    assert_eq!(restored.tables().len(), 1);
    assert_eq!(
        restored.tables()[&temporary].persistence,
        RelationPersistence::Temporary
    );
    assert_eq!(
        restored.security()[&temporary],
        current.security()[&temporary]
    );
    assert!(!restored.tables().contains_key(&removed));
    assert!(!restored.security().contains_key(&removed));
    let mut latest = table("public.latest", RelationPersistence::Permanent);
    latest.object_id = [9; 16];
    let latest_relation = RelationIdentity::new("public", "latest");
    let mut latest_catalog = ForeignCatalog::default();
    latest_catalog
        .insert(latest_relation.clone(), latest, security)
        .unwrap();
    let merged = crate::catalog::security::relation_authority::merge_private_foreign(
        None,
        &current,
        latest_catalog.into(),
    )
    .unwrap();
    assert_eq!(merged.tables().len(), 2);
    assert!(merged.tables().contains_key(&latest_relation));
    assert_eq!(
        merged.security()[&temporary],
        current.security()[&temporary]
    );
    assert!(!merged.tables().contains_key(&removed));
    let invalid = BTreeMap::from([(temporary.clone(), current.tables()[&temporary].clone())]);
    assert!(admit_restored(&invalid, &BTreeMap::new(), current.budget()).is_err());
}

#[test]
fn refresh_retains_an_unbound_declaration_until_its_server_is_attached() {
    let relation = RelationIdentity::new("public", "declared");
    let security = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
    let mut declared = table("public.declared", RelationPersistence::Permanent);
    declared.server_reference = None;
    let object_id = declared.object_id;
    let mut current = ForeignCatalog::default();
    current
        .insert(relation.clone(), declared, security.clone())
        .unwrap();
    let mut restored = ForeignCatalog::default();
    merge_session_relations(&current, &mut restored).unwrap();
    assert_eq!(restored.tables()[&relation].object_id, object_id);
    assert_eq!(restored.security()[&relation], security);
    let mut attached = current.tables()[&relation].clone();
    attached.server_reference =
        table("public.declared", RelationPersistence::Permanent).server_reference;
    current.replace_table(relation.clone(), attached).unwrap();
    let mut restored = ForeignCatalog::default();
    merge_session_relations(&current, &mut restored).unwrap();
    assert!(restored.tables().is_empty());
    assert!(restored.security().is_empty());
}
