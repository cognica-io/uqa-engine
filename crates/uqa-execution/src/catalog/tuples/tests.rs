//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical tuple identity is independent of OIDs, field equality and current heads.

use super::*;
use std::sync::Arc;
use uqa_storage::{KeyValueCatalog, KeyValueStorageBackend, MemoryKeyValueStore};

fn session() -> PersistentStorageSession {
    let store = Arc::new(MemoryKeyValueStore::new());
    PersistentStorageSession::new(
        Arc::new(KeyValueCatalog::new(store.clone())),
        Arc::new(KeyValueStorageBackend::new(store)),
    )
}

fn object(id: u8) -> CatalogObjectIdentity {
    CatalogObjectIdentity {
        object_id: [id; 16],
        oid: 16_384 + i64::from(id),
    }
}

fn payload(value: &str) -> Box<RawValue> {
    serde_json::value::to_raw_value(&serde_json::json!({"name":value})).unwrap()
}

fn rows(session: &PersistentStorageSession) -> Vec<(PhysicalTuple, String)> {
    let mut rows = Vec::new();
    visit(
        &*session.catalog,
        CatalogOidClass::Constraint,
        &mut |tuple, json| {
            let value: serde_json::Value = serde_json::from_str(json.get())?;
            rows.push((tuple, value["name"].as_str().unwrap().to_owned()));
            Ok(())
        },
    )
    .unwrap();
    rows.sort_by_key(|(tuple, _)| tuple.identity().oid);
    rows
}

fn sqlstate(error: &StorageBackendError) -> String {
    uqa_sql::catalog::errors::storage_error("catalog tuples", error)
        .sqlstate()
        .unwrap()
        .to_owned()
}

#[test]
fn replacements_preserve_logical_identity_and_replace_only_the_selected_physical_tuple() {
    let session = session();
    session.backend.begin_transaction().unwrap();
    let class = CatalogOidClass::Constraint;
    let original = insert(&session, class, object(1), &payload("original")).unwrap();
    let unrelated = insert(&session, class, object(2), &payload("unrelated")).unwrap();
    let checkpoint = StorageSavepointId::allocate();
    session.backend.savepoint(checkpoint).unwrap();
    let renamed = replace(&session, class, original, &payload("renamed")).unwrap();
    let equal = replace(&session, class, renamed, &payload("renamed")).unwrap();
    assert_eq!(original.generation(), 0);
    assert_eq!(renamed.generation(), 1);
    assert_eq!(equal.generation(), 2);
    assert_ne!(original, renamed);
    assert_ne!(renamed, equal);
    assert_eq!(original.identity(), equal.identity());
    assert_eq!(
        rows(&session),
        [(equal, "renamed".into()), (unrelated, "unrelated".into())]
    );
    for error in [
        replace(&session, class, original, &payload("stale")).unwrap_err(),
        remove(&session, class, renamed).unwrap_err(),
        insert(&session, class, object(1), &payload("duplicate")).unwrap_err(),
    ] {
        assert_eq!(sqlstate(&error), "40001");
    }
    assert_eq!(current(&session, class, object(1)).unwrap(), Some(equal));
    session.backend.rollback_to_savepoint(checkpoint).unwrap();
    assert_eq!(
        rows(&session),
        [
            (original, "original".into()),
            (unrelated, "unrelated".into())
        ]
    );
    remove(&session, class, original).unwrap();
    assert_eq!(rows(&session), [(unrelated, "unrelated".into())]);
    session.backend.commit_transaction().unwrap();
    session.backend.begin_read_transaction().unwrap();
    assert_eq!(current(&session, class, object(1)).unwrap(), None);
    session.backend.rollback_transaction().unwrap();
}

#[test]
fn physical_visits_preserve_same_oid_rows_without_joining_current_heads() {
    let session = session();
    session.backend.begin_transaction().unwrap();
    let class = CatalogOidClass::Constraint;
    let original = insert(&session, class, object(1), &payload("original")).unwrap();
    let original_record = session
        .catalog
        .get_metadata(&original.key())
        .unwrap()
        .unwrap();
    let current = replace(&session, class, original, &payload("own")).unwrap();
    // Supply the two exact physical rows an old committed/private view exposes; storage owns their MVCC selection.
    session
        .catalog
        .set_metadata(&original.key(), &original_record)
        .unwrap();
    let rows = rows(&session);
    assert_eq!(rows.len(), 2);
    assert!(rows.contains(&(original, "original".into())));
    assert!(rows.contains(&(current, "own".into())));
    assert!(rows
        .iter()
        .all(|(tuple, _)| tuple.identity().oid == object(1).oid));
    session.backend.rollback_transaction().unwrap();
}

#[test]
fn publication_requires_an_active_transaction_and_failure_restores_all_writes() {
    let session = session();
    let class = CatalogOidClass::Constraint;
    assert!(current(&session, class, object(1)).is_err());
    assert!(insert(&session, class, object(1), &payload("forbidden")).is_err());
    assert!(rows(&session).is_empty());
    session.backend.begin_transaction().unwrap();
    let original = insert(&session, class, object(1), &payload("original")).unwrap();
    let cancellation = uqa_core::CancellationToken::new();
    let result: StorageBackendResult<()> = transaction::atomic(&session, || {
        remove(&session, class, original)?;
        insert(&session, class, object(2), &payload("discarded"))?;
        cancellation.cancel();
        cancellation.check()?;
        Ok(())
    });
    assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
    assert!(session.backend.in_transaction());
    assert_eq!(rows(&session), [(original, "original".into())]);
    session.backend.commit_transaction().unwrap();
    assert_eq!(rows(&session), [(original, "original".into())]);
}

#[test]
fn invalid_format_keys_and_definition_links_fail_before_publication() {
    let session = session();
    let class = CatalogOidClass::Constraint;
    session.backend.begin_transaction().unwrap();
    let tuple = insert(&session, class, object(1), &payload("original")).unwrap();
    let json = session.catalog.get_metadata(&tuple.key()).unwrap().unwrap();
    for replacement in [
        json.replace("\"format\":1", "\"format\":2"),
        json.replace("\"class_oid\":2606", "\"class_oid\":1259"),
        json.replace("\"oid\":16385", "\"oid\":0"),
        "{".into(),
    ] {
        session
            .catalog
            .set_metadata(&tuple.key(), &replacement)
            .unwrap();
        let error = replace(&session, class, tuple, &payload("invalid")).unwrap_err();
        assert_eq!(sqlstate(&error), "XX001");
        assert_eq!(
            session.catalog.get_metadata(&tuple.key()).unwrap().unwrap(),
            replacement
        );
    }
    session.catalog.set_metadata(&tuple.key(), &json).unwrap();
    let other = PhysicalTuple {
        object: object(2),
        ..tuple
    };
    session
        .catalog
        .set_metadata(
            &head_key(class, object(1)),
            &serde_json::to_string(&other).unwrap(),
        )
        .unwrap();
    assert_eq!(
        sqlstate(&current(&session, class, object(1)).unwrap_err()),
        "XX001"
    );
    session.backend.rollback_transaction().unwrap();
}

#[test]
fn typed_nodes_round_trip_without_rendered_sql_or_payload_collection() {
    use uqa_sql::catalog::node_tree::{Field, Node};
    let session = session();
    session.backend.begin_transaction().unwrap();
    let node = Node::new(
        "CONST",
        [
            ("consttype", Field::from(23_i64)),
            (
                "constvalue",
                Field::Datum {
                    length: 4,
                    bytes: vec![0, 128, 255, 1, 0, 0, 0, 0],
                },
            ),
        ],
    );
    let fields = uqa_sql::ResultRow::from([
        ("oid".into(), uqa_core::Value::Int(object(1).oid)),
        ("conname".into(), uqa_core::Value::Str("owned_check".into())),
    ]);
    let stored = constraint::ConstraintTuple::new(fields.clone(), Some(node.clone()));
    let payload = serde_json::value::to_raw_value(&stored).unwrap();
    let tuple = insert(&session, CatalogOidClass::Constraint, object(1), &payload).unwrap();
    visit(
        &*session.catalog,
        CatalogOidClass::Constraint,
        &mut |identity, raw| {
            assert_eq!(identity, tuple);
            let retained: constraint::ConstraintTuple = serde_json::from_str(raw.get())?;
            assert_eq!(retained.expression(), Some(&node));
            let mut expected = fields.clone();
            expected.insert("conbin".into(), uqa_core::Value::Str(node.to_string()));
            assert_eq!(retained.into_row(), expected);
            Ok(())
        },
    )
    .unwrap();
    session.backend.rollback_transaction().unwrap();
}

#[test]
fn tuple_visitors_borrow_large_payloads_and_preserve_callback_errors() {
    let session = session();
    session.backend.begin_transaction().unwrap();
    let payload = payload(&"x".repeat(2 << 20));
    let tuple = insert(&session, CatalogOidClass::Constraint, object(1), &payload).unwrap();
    let mut count = 0;
    let allocations = allocation_counter::measure(|| {
        assert_eq!(
            current(&session, CatalogOidClass::Constraint, object(1)).unwrap(),
            Some(tuple)
        );
        visit(
            &*session.catalog,
            CatalogOidClass::Constraint,
            &mut |_, raw| {
                assert_eq!(raw.get().len(), payload.get().len());
                count += 1;
                Ok(())
            },
        )
        .unwrap();
    });
    assert!(allocations.bytes_max < 8 * 1024, "{allocations:?}");
    assert_eq!(count, 1);
    let error = visit(
        &*session.catalog,
        CatalogOidClass::Constraint,
        &mut |_, _| {
            Err(StorageBackendError::Other(
                "selected callback failure".into(),
            ))
        },
    )
    .unwrap_err();
    assert!(
        matches!(error, StorageBackendError::Other(ref message) if message == "selected callback failure")
    );
    session.backend.rollback_transaction().unwrap();
}
