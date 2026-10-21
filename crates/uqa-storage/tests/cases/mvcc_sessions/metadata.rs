//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[path = "metadata/observed.rs"]
mod observed;
use uqa_storage::{CatalogFacade, KeyValueCatalog};

#[test]
fn foreign_table_provenance_preserves_exact_names_rename_deletion_and_undo() {
    let persistence = Persistence::new();
    let first = Arc::new(persistence.session(1 << 24));
    let second = Arc::new(persistence.session(1 << 24));
    let a = KeyValueCatalog::new(first.clone());
    let b = KeyValueCatalog::new(second.clone());
    let schema = "tenant:%_\0日本語";
    a.save_schema(schema).unwrap();
    a.save_schema("other").unwrap();
    a.save_foreign_server("remote", "memory_fdw", "{}").unwrap();
    let saved = uqa_core::RelationIdentity::new(schema, "saved:%_\0");
    let neighbor = uqa_core::RelationIdentity::new(schema, "saved:%_\0next");
    let other = uqa_core::RelationIdentity::new("other", &saved.name);
    let renamed = uqa_core::RelationIdentity::new(schema, "renamed");
    let created = uqa_core::RelationIdentity::new(schema, "created");
    let peer = uqa_core::RelationIdentity::new(schema, "peer");
    let row = |relation: &uqa_core::RelationIdentity| uqa_storage::ForeignTableRow {
        relation: relation.clone(),
        security: uqa_storage::RelationSecurityRow::legacy("uqa"),
        server_name: "remote".into(),
        columns_json: "[]".into(),
        options_json: "{}".into(),
    };
    for relation in [&saved, &neighbor, &other] {
        a.save_foreign_table(&row(relation)).unwrap();
        assert!(!a.foreign_table_has_private_changes(relation).unwrap());
    }
    first.begin_transaction().unwrap();
    first.savepoint("foreign_tables").unwrap();
    a.save_foreign_table(&row(&saved)).unwrap();
    assert!(a.foreign_table_has_private_changes(&saved).unwrap());
    assert!(!a.view_has_private_changes(&saved).unwrap());
    a.rename_foreign_table(&saved, &renamed).unwrap();
    a.save_foreign_table(&row(&created)).unwrap();
    a.drop_foreign_table(&renamed).unwrap();
    for relation in [&saved, &renamed, &created] {
        assert!(a.foreign_table_has_private_changes(relation).unwrap());
        assert!(!b.foreign_table_has_private_changes(relation).unwrap());
    }
    for relation in [
        &neighbor,
        &other,
        &uqa_core::RelationIdentity::new(schema, "saved"),
    ] {
        assert!(!a.foreign_table_has_private_changes(relation).unwrap());
    }
    b.save_foreign_table(&row(&peer)).unwrap();
    first
        .refresh_transaction_snapshot(&uqa_core::CancellationToken::new())
        .unwrap();
    assert!(a.foreign_table_has_private_changes(&renamed).unwrap());
    assert!(!a.foreign_table_has_private_changes(&peer).unwrap());
    first.rollback_to_savepoint("foreign_tables").unwrap();
    for relation in [&saved, &renamed, &created] {
        assert!(!a.foreign_table_has_private_changes(relation).unwrap());
    }
    a.drop_foreign_table(&saved).unwrap();
    first.commit_transaction().unwrap();
    assert!(!a.foreign_table_has_private_changes(&saved).unwrap());
    let rows = b.load_foreign_tables().unwrap();
    assert!(!rows.iter().any(|row| row.relation == saved));
    assert!(rows.iter().any(|row| row.relation == peer));
}

#[test]
fn borrowed_metadata_visits_retain_private_and_committed_boundaries() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 24));
    uqa_storage::key_value::conformance::verify_catalog_metadata_visits(&store).unwrap();
}

#[test]
fn schema_private_provenance_preserves_deletions_and_exact_names_through_undo() {
    let persistence = Persistence::new();
    let first = Arc::new(persistence.session(1 << 24));
    let second = Arc::new(persistence.session(1 << 24));
    let a = KeyValueCatalog::new(first.clone());
    let b = KeyValueCatalog::new(second.clone());
    let name = "tenant:%_\0日本語";
    let neighbor = format!("{name}:next");
    a.save_schema(name).unwrap();
    a.save_schema(&neighbor).unwrap();
    assert!(!a.schema_has_private_changes(name).unwrap());
    first.begin_transaction().unwrap();
    first.savepoint("schemas").unwrap();
    a.drop_schema(name).unwrap();
    a.save_schema("created").unwrap();
    for schema in [name, "created"] {
        assert!(a.schema_has_private_changes(schema).unwrap());
        assert!(!b.schema_has_private_changes(schema).unwrap());
    }
    for schema in ["tenant", neighbor.as_str(), "absent"] {
        assert!(!a.schema_has_private_changes(schema).unwrap());
    }
    b.save_schema("peer").unwrap();
    first
        .refresh_transaction_snapshot(&uqa_core::CancellationToken::new())
        .unwrap();
    assert!(a.schema_has_private_changes(name).unwrap());
    assert!(!a.schema_has_private_changes("peer").unwrap());
    first.rollback_to_savepoint("schemas").unwrap();
    for schema in [name, "created"] {
        assert!(!a.schema_has_private_changes(schema).unwrap());
    }
    a.drop_schema(name).unwrap();
    first.commit_transaction().unwrap();
    assert!(!a.schema_has_private_changes(name).unwrap());
    assert!(!b
        .load_schemas()
        .unwrap()
        .iter()
        .any(|schema| schema == name));
    assert!(b.load_schemas().unwrap().contains(&"peer".into()));
}

#[test]
fn metadata_private_provenance_and_literal_prefixes_follow_savepoints_and_refresh() {
    let persistence = Persistence::new();
    let first = Arc::new(persistence.session(1 << 24));
    let second = Arc::new(persistence.session(1 << 24));
    let a = KeyValueCatalog::new(first.clone());
    let b = KeyValueCatalog::new(second.clone());
    a.set_metadata("acl:table", "committed").unwrap();
    assert!(!a.metadata_has_private_changes("acl:table").unwrap());
    first.begin_transaction().unwrap();
    a.set_metadata("acl:table", "private").unwrap();
    assert!(a.metadata_has_private_changes("acl:table").unwrap());
    assert!(!a.metadata_has_private_changes("acl:tab").unwrap());
    assert!(!a.metadata_has_private_changes("acl:table:column").unwrap());
    first.savepoint("attribute").unwrap();
    a.set_metadata("acl:table:column", "attribute").unwrap();
    assert!(a.metadata_has_private_changes("acl:table:column").unwrap());
    b.set_metadata("acl:other", "external").unwrap();
    first
        .refresh_transaction_snapshot(&uqa_core::CancellationToken::new())
        .unwrap();
    assert!(!a.metadata_has_private_changes("acl:other").unwrap());
    assert!(a.metadata_has_private_changes("acl:table").unwrap());
    first.rollback_to_savepoint("attribute").unwrap();
    assert!(!a.metadata_has_private_changes("acl:table:column").unwrap());
    assert!(a.metadata_has_private_changes("acl:table").unwrap());
    first.commit_transaction().unwrap();
    assert!(!a.metadata_has_private_changes("acl:table").unwrap());
    a.set_metadata("acl:%_\0日本語", "literal").unwrap();
    a.set_metadata("acl:other", "other").unwrap();
    assert_eq!(
        a.metadata_with_prefix("acl:%_\0").unwrap(),
        vec![("acl:%_\0日本語".into(), "literal".into())]
    );
    assert_eq!(a.metadata_with_prefix("absent").unwrap().len(), 0);
}

#[test]
fn metadata_deletion_preserves_independent_commits_and_savepoint_undo() {
    let persistence = Persistence::new();
    let first = Arc::new(persistence.session(1 << 24));
    let second = Arc::new(persistence.session(1 << 24));
    let a = KeyValueCatalog::new(first.clone());
    let b = KeyValueCatalog::new(second.clone());
    let key = "role:%_\0日本語";
    let neighbor = format!("{key}:child");
    a.set_metadata(key, "kept").unwrap();
    a.set_metadata(&neighbor, "neighbor").unwrap();
    first.begin_transaction().unwrap();
    first.savepoint("removed").unwrap();
    a.delete_metadata(key).unwrap();
    assert!(a.metadata_has_private_changes(key).unwrap());
    assert!(a.get_metadata(key).unwrap().is_none());
    assert_eq!(b.get_metadata(key).unwrap().as_deref(), Some("kept"));
    b.set_metadata("role:other", "external").unwrap();
    first
        .refresh_transaction_snapshot(&uqa_core::CancellationToken::new())
        .unwrap();
    assert!(a.get_metadata(key).unwrap().is_none());
    assert_eq!(
        a.get_metadata(&neighbor).unwrap().as_deref(),
        Some("neighbor")
    );
    first.rollback_to_savepoint("removed").unwrap();
    assert!(!a.metadata_has_private_changes(key).unwrap());
    assert_eq!(a.get_metadata(key).unwrap().as_deref(), Some("kept"));
    a.delete_metadata(key).unwrap();
    first.commit_transaction().unwrap();
    assert!(b.get_metadata(key).unwrap().is_none());
    assert_eq!(
        b.get_metadata("role:other").unwrap().as_deref(),
        Some("external")
    );
    assert!(!a.metadata_has_private_changes(key).unwrap());
}
