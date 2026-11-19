//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Catalog, GraphSnapshot, ManagedConnection};
use std::path::Path;
use uqa_storage::mvcc::VersionedSessionOptions;

fn open(path: &Path, native: bool) -> Catalog {
    let connection = ManagedConnection::open(path).unwrap();
    if native {
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
    }
    Catalog::open(connection).unwrap()
}

fn empty_graph() -> GraphSnapshot {
    GraphSnapshot {
        vertices: Vec::new(),
        edges: Vec::new(),
        label_registry_json: "{}".into(),
    }
}

#[rstest::rstest]
#[case::sqlite(false)]
#[case::native(true)]
fn graph_registration_retains_only_unrecorded_legacy_owners_across_rollback_and_reopen(
    #[case] native: bool,
) {
    const OWNER: &str = "company::archive";
    const LEGACY: &str = "company::archive::idx";
    const BUILT: &str = "company::archive::built";
    const OTHER: &str = "companyX::archive::idx";
    const DEFINITION: &str = r#"[["edge"]]"#;
    const SEQUENCE: &str = r#"["edge"]"#;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.db");
    let catalog = open(&path, native);
    catalog.save_named_graph(OWNER).unwrap();
    catalog.save_named_graph("companyX::archive").unwrap();
    for name in [LEGACY, BUILT, OTHER] {
        catalog.save_path_index(name, DEFINITION).unwrap();
    }
    catalog
        .save_path_index_pairs(LEGACY, SEQUENCE, &[(7, 11)])
        .unwrap();
    catalog
        .finish_path_index_data(BUILT, OWNER, DEFINITION)
        .unwrap();
    let definitions = catalog.load_path_indexes().unwrap();
    catalog.conn.begin_transaction().unwrap();
    catalog.save_named_graph("company").unwrap();
    assert_eq!(
        catalog.path_index_graph(LEGACY).unwrap().as_deref(),
        Some(OWNER)
    );
    catalog.conn.rollback_transaction().unwrap();
    assert!(!catalog.named_graph_exists("company").unwrap());
    assert_eq!(catalog.path_index_graph(LEGACY).unwrap(), None);

    catalog
        .replace_named_graph("company", &empty_graph())
        .unwrap();
    drop(catalog);
    let catalog = open(&path, native);
    assert!(catalog.named_graph_exists("company").unwrap());
    assert_eq!(
        catalog.path_index_graph(LEGACY).unwrap().as_deref(),
        Some(OWNER)
    );
    assert_eq!(catalog.path_index_graph(OTHER).unwrap(), None);
    assert!(!catalog
        .path_index_data_is_current(LEGACY, DEFINITION)
        .unwrap());
    assert!(catalog
        .path_index_data_is_current(BUILT, DEFINITION)
        .unwrap());
    assert_eq!(
        catalog.path_index_pairs(LEGACY, SEQUENCE, None, 2).unwrap(),
        [(7, 11)]
    );
    assert_eq!(catalog.load_path_indexes().unwrap(), definitions);
    catalog.drop_named_graph_data("company").unwrap();
    assert_eq!(catalog.load_path_indexes().unwrap(), definitions);
}

#[rstest::rstest]
#[case::sqlite(false)]
#[case::native(true)]
fn ambiguous_legacy_owner_aborts_graph_creation_and_earlier_owner_changes(#[case] native: bool) {
    let directory = tempfile::tempdir().unwrap();
    let catalog = open(&directory.path().join("catalog.db"), native);
    let first = "company::archive::aa";
    let ambiguous = "company::archive::zz::idx";
    catalog.save_named_graph("company").unwrap();
    catalog.save_named_graph("company::archive::zz").unwrap();
    catalog.save_path_index(first, "[]").unwrap();
    catalog.save_path_index(ambiguous, "[]").unwrap();
    let error = catalog.save_named_graph("company::archive").unwrap_err();
    assert!(
        error.to_string().contains("ambiguous path-index owner"),
        "{error}"
    );
    assert!(!catalog.named_graph_exists("company::archive").unwrap());
    assert_eq!(catalog.path_index_graph(first).unwrap(), None);
    assert_eq!(catalog.path_index_graph(ambiguous).unwrap(), None);
}

#[rstest::rstest]
#[case::index(false)]
#[case::owner(true)]
fn concurrent_native_removal_cannot_publish_a_stale_legacy_owner(#[case] remove_owner: bool) {
    let directory = tempfile::tempdir().unwrap();
    let catalog = open(&directory.path().join("catalog.db"), true);
    let peer = Catalog::open(catalog.conn.new_session()).unwrap();
    let index = "company::archive::idx";
    catalog.save_named_graph("company::archive").unwrap();
    catalog.save_path_index(index, "[]").unwrap();
    catalog.conn.begin_transaction().unwrap();
    catalog.save_named_graph("company").unwrap();
    if remove_owner {
        peer.drop_named_graph("company::archive").unwrap();
    } else {
        peer.drop_path_index(index).unwrap();
    }
    assert!(catalog.conn.commit_transaction().is_err());
    catalog.conn.rollback_transaction().unwrap();
    assert!(!peer.named_graph_exists("company").unwrap());
    assert_eq!(peer.load_path_indexes().unwrap().is_empty(), !remove_owner);
    assert_eq!(peer.path_index_graph(index).unwrap(), None);
}
