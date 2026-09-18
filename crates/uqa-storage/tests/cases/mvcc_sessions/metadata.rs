//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::{CatalogFacade, KeyValueCatalog};

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
    assert!(a.metadata_with_prefix("absent").unwrap().is_empty());
}
