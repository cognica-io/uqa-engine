//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn native_metadata_private_provenance_tracks_exact_keys_and_savepoint_undo() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    let other = Catalog::open(connection.new_session()).unwrap();
    catalog.set_metadata("acl:table", "committed").unwrap();
    assert!(!catalog.metadata_has_private_changes("acl:table").unwrap());
    connection.begin_transaction().unwrap();
    catalog.set_metadata("acl:table", "private").unwrap();
    assert!(catalog.metadata_has_private_changes("acl:table").unwrap());
    for key in ["acl:tab", "acl:table:column"] {
        assert!(!catalog.metadata_has_private_changes(key).unwrap());
    }
    connection.savepoint("attribute").unwrap();
    catalog.set_metadata("acl:table:column", "column").unwrap();
    assert!(catalog
        .metadata_has_private_changes("acl:table:column")
        .unwrap());
    other.set_metadata("acl:other", "external").unwrap();
    connection.rollback_to_savepoint("attribute").unwrap();
    assert!(!catalog
        .metadata_has_private_changes("acl:table:column")
        .unwrap());
    assert!(catalog.metadata_has_private_changes("acl:table").unwrap());
    connection.commit_transaction().unwrap();
    assert!(!catalog.metadata_has_private_changes("acl:table").unwrap());
    assert_eq!(
        catalog.get_metadata("acl:other").unwrap().as_deref(),
        Some("external")
    );
}

#[test]
fn metadata_prefix_reads_preserve_literal_nul_unicode_and_wildcards() {
    for native in [false, true] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let catalog = Catalog::open(connection.clone()).unwrap();
        if native {
            connection
                .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
                .unwrap();
        }
        catalog.set_metadata("acl:%_\0日本語", "literal").unwrap();
        catalog.set_metadata("acl:other", "other").unwrap();
        assert_eq!(
            catalog.metadata_with_prefix("acl:%_\0").unwrap(),
            vec![("acl:%_\0日本語".into(), "literal".into())]
        );
        assert!(catalog.metadata_with_prefix("absent").unwrap().is_empty());
    }
}

#[test]
fn metadata_deletion_is_exact_transactional_and_visible_as_a_private_change() {
    for native in [false, true] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let catalog = Catalog::open(connection.clone()).unwrap();
        if native {
            connection
                .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
                .unwrap();
        }
        let key = "role:%_\0日本語";
        let neighbor = format!("{key}:child");
        catalog.set_metadata(key, "kept").unwrap();
        catalog.set_metadata(&neighbor, "neighbor").unwrap();
        connection.begin_transaction().unwrap();
        connection.savepoint("removed").unwrap();
        catalog.delete_metadata(key).unwrap();
        assert!(catalog.get_metadata(key).unwrap().is_none());
        assert_eq!(catalog.metadata_has_private_changes(key).unwrap(), native);
        assert_eq!(
            catalog.get_metadata(&neighbor).unwrap().as_deref(),
            Some("neighbor")
        );
        connection.rollback_to_savepoint("removed").unwrap();
        assert_eq!(catalog.get_metadata(key).unwrap().as_deref(), Some("kept"));
        assert!(!catalog.metadata_has_private_changes(key).unwrap());
        catalog.delete_metadata(key).unwrap();
        connection.commit_transaction().unwrap();
        assert!(catalog.get_metadata(key).unwrap().is_none());
        assert!(!catalog.metadata_has_private_changes(key).unwrap());
        catalog.delete_metadata(key).unwrap();
        if native {
            assert!(catalog.delete_metadata("schema_version").is_err());
            assert!(catalog.get_metadata("schema_version").unwrap().is_some());
        }
    }
}

#[test]
fn native_graph_registry_metadata_deletion_retains_definition_fences_and_path_invalidation() {
    for deletion_wins in [false, true] {
        let a = ManagedConnection::open_in_memory().unwrap();
        let first = Catalog::open(a.clone()).unwrap();
        a.bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
            .unwrap();
        let b = a.new_session();
        let second = Catalog::open(b.clone()).unwrap();
        first.save_named_graph("g").unwrap();
        first.save_vertex(1, "node", "{}").unwrap();
        first.save_graph_membership("vertex", 1, "g").unwrap();
        first.set_metadata("graph_label_registry::g", "{}").unwrap();
        first.save_path_index("paths", "[]").unwrap();
        first.finish_path_index_data("paths", "g", "[]").unwrap();
        assert!(first.path_index_data_is_current("paths", "[]").unwrap());
        a.begin_transaction().unwrap();
        b.begin_transaction().unwrap();
        first.delete_metadata("graph_label_registry::g").unwrap();
        assert!(!first.path_index_data_is_current("paths", "[]").unwrap());
        second
            .save_vertex(1, "node", r#"{"changed":true}"#)
            .unwrap();
        let (winner, loser) = if deletion_wins { (&a, &b) } else { (&b, &a) };
        winner.commit_transaction().unwrap();
        assert!(loser.commit_transaction().is_err());
        loser.rollback_transaction().unwrap();
        assert_eq!(
            first
                .get_metadata("graph_label_registry::g")
                .unwrap()
                .is_none(),
            deletion_wins
        );
    }
}
