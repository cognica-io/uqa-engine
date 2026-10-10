//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::cell::Cell;
use std::collections::BTreeMap;

use super::*;
use crate::{Catalog, ManagedConnection, SQLiteDocumentStore};
use uqa_core::Value;
use uqa_storage::{mvcc::VersionedSessionOptions, DocumentStore};

thread_local! {
    static VISITED: Cell<usize> = const { Cell::new(0) };
}

pub(super) fn visited() {
    VISITED.set(VISITED.get() + 1);
}

#[test]
fn native_cache_projection_visits_one_scope_for_many_private_documents() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    let mut document = SQLiteDocumentStore::new(connection.clone(), "docs");
    document
        .put(0, BTreeMap::from([("n".into(), Value::Int(0))]))
        .unwrap();
    connection
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 1 << 20,
        })
        .unwrap();
    let before = catalog.cache_revisions().unwrap();
    assert!(before.table_data_commits.contains_key("docs"));
    connection.begin_transaction().unwrap();
    let fields = BTreeMap::from([("n".into(), Value::Str("v".repeat(512)))]);
    for id in 1..=2048 {
        document.put(id, fields.clone()).unwrap();
        if [32, 256, 2048].contains(&id) {
            VISITED.set(0);
            let current = catalog.cache_revisions().unwrap();
            assert_eq!(
                VISITED.get(),
                1,
                "projection work must depend on owners, not rows"
            );
            assert_ne!(current.table_data["docs"], before.table_data["docs"]);
            assert_eq!(
                current.table_data_commits["docs"],
                before.table_data_commits["docs"]
            );
            assert_eq!(current.table_catalog, before.table_catalog);
            assert_eq!(current.registries, before.registries);
        }
    }
    let captured = connection.native_snapshot().unwrap().unwrap();
    let saved = captured.cache_revisions().unwrap();
    connection.savepoint("keep").unwrap();
    document.delete(1).unwrap();
    assert_ne!(catalog.cache_revisions().unwrap(), saved);
    connection.rollback_to_savepoint("keep").unwrap();
    assert_eq!(catalog.cache_revisions().unwrap(), saved);
    document
        .put(1, BTreeMap::from([("n".into(), Value::Int(1))]))
        .unwrap();
    assert_ne!(catalog.cache_revisions().unwrap(), saved);
    assert_eq!(captured.cache_revisions().unwrap(), saved);
    connection.rollback_transaction().unwrap();
    assert_eq!(catalog.cache_revisions().unwrap(), before);
    assert_eq!(captured.cache_revisions().unwrap(), saved);
}

#[test]
fn native_revision_summary_follows_command_refresh_and_its_savepoint_undo() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("revision-refresh.db");
    let first = ManagedConnection::open(&path).unwrap();
    let catalog = Catalog::open(first.clone()).unwrap();
    let mut documents = SQLiteDocumentStore::new(first.clone(), "docs");
    documents
        .put(1, BTreeMap::from([("n".into(), Value::Int(1))]))
        .unwrap();
    first
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let second = ManagedConnection::open(&path).unwrap();
    second
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let mut other = SQLiteDocumentStore::new(second, "docs");
    first.begin_transaction().unwrap();
    for id in 2..=32 {
        documents
            .put(id, BTreeMap::from([("n".into(), Value::Int(2))]))
            .unwrap();
    }
    let captured = first.native_snapshot().unwrap().unwrap();
    let saved = captured.cache_revisions().unwrap();
    first.savepoint("before_refresh").unwrap();
    other
        .put(1, BTreeMap::from([("n".into(), Value::Int(9))]))
        .unwrap();
    first
        .refresh_transaction_snapshot(&uqa_core::CancellationToken::new())
        .unwrap();
    VISITED.set(0);
    let refreshed = catalog.cache_revisions().unwrap();
    assert_ne!(refreshed, saved);
    assert_ne!(
        refreshed.table_data_commits["docs"],
        saved.table_data_commits["docs"]
    );
    assert_eq!(VISITED.get(), 1, "rebasing must restore grouped revisions");
    assert_eq!(documents.get(1).unwrap().unwrap()["n"], Value::Int(9));
    assert_eq!(captured.cache_revisions().unwrap(), saved);
    first.rollback_to_savepoint("before_refresh").unwrap();
    assert_eq!(catalog.cache_revisions().unwrap(), saved);
    assert_eq!(documents.get(1).unwrap().unwrap()["n"], Value::Int(1));
    first.rollback_transaction().unwrap();
}

#[test]
fn native_revision_scopes_keep_names_and_separate_family_owner_and_generation() {
    let control = StorageReadControl::with_limit(1 << 20);
    let database = NativeRecordOwner::Database(uqa_storage::mvcc::DatabaseId::from_bytes([1; 16]));
    let owner = |identity, generation| NativeRecordOwner::Object {
        identity: [identity; 16],
        generation: [generation; 16],
    };
    let key = |family, owner, components: &[ValueRef<'_>]| {
        NativeRecordIdentity::new(family, owner)
            .unwrap()
            .encode_prefix(components, &control)
            .unwrap()
    };
    let mut scopes = std::collections::BTreeSet::new();
    for family in [
        Family::Documents,
        Family::ColumnStats,
        Family::BtreeIndexEntries,
    ] {
        for owner in [owner(1, 1), owner(2, 1), owner(1, 2)] {
            let prefix = key(family, owner, &[]);
            for id in [1, 2] {
                let value = if family == Family::Documents {
                    ValueRef::Integer(id)
                } else {
                    text(if id == 1 { "a" } else { "b" })
                };
                let record = key(family, owner, &[value]);
                assert_eq!(
                    NativeRecordIdentity::revision_scope(&record),
                    Some(&*prefix)
                );
            }
            assert!(scopes.insert(prefix.to_vec()));
        }
    }
    for family in [
        Family::Metadata,
        Family::NamedGraphs,
        Family::GraphMembership,
        Family::GraphVertices,
        Family::GraphEdges,
    ] {
        for name in ["a", "bb", "a\0日本語"] {
            let value = if matches!(family, Family::GraphVertices | Family::GraphEdges) {
                ValueRef::Integer(i64::try_from(name.len()).unwrap())
            } else {
                text(name)
            };
            let record = key(family, database, &[value]);
            assert_eq!(
                NativeRecordIdentity::revision_scope(&record),
                Some(&*record)
            );
            assert!(scopes.insert(record.to_vec()));
        }
    }
    for family in [
        Family::Vectors,
        Family::HNSWIndexes,
        Family::HNSWNodes,
        Family::HNSWEdges,
        Family::IVFIndexes,
        Family::IVFCentroids,
        Family::IVFAssignments,
    ] {
        for owner in [owner(1, 1), owner(2, 1), owner(1, 2)] {
            for field in ["a", "ab", "a\0日本語"] {
                let prefix = key(family, owner, &[text(field)]);
                for id in [1, 2] {
                    let mut components = vec![text(field)];
                    if family.layout().identity_columns.len() > 1 {
                        components.push(ValueRef::Integer(id));
                    }
                    let record = key(family, owner, &components);
                    assert_eq!(
                        NativeRecordIdentity::revision_scope(&record),
                        Some(&*prefix)
                    );
                }
                assert!(scopes.insert(prefix.to_vec()));
            }
        }
    }
    for family in [
        Family::TableOwners,
        Family::GraphLookups,
        Family::GraphPathPairs,
        Family::GraphPathIndexState,
    ] {
        let record = key(family, database, &[]);
        assert!(NativeRecordIdentity::revision_scope(&record).is_none());
    }
    assert_eq!(
        NativeRecordIdentity::revision_scope(b"invalid"),
        Some(b"invalid".as_slice())
    );
}
