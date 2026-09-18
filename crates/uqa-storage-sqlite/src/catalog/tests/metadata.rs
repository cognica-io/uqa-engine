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
