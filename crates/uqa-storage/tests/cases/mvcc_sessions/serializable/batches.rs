//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated logical intents share record rollback and cannot change participants.

use super::*;

fn batch_write(session: &VersionedKeyValueStore, key: &[u8]) {
    session
        .with_mutation(&mut |_, batch| {
            assert!(batch.serializable_participant().is_some());
            batch.observe_serializable_write(predicate(key))?;
            batch.put(key, b"batch")
        })
        .unwrap();
}

#[test]
fn evaluated_writes_detect_cycles_without_reentering_the_session() {
    let persistence = Persistence::new();
    let (a, ra) = start(&persistence, false);
    let (b, rb) = start(&persistence, false);
    read(&ra, b"left");
    read(&rb, b"right");
    batch_write(&a, b"right");
    batch_write(&b, b"left");
    a.commit_transaction().unwrap();
    b.commit_transaction().unwrap_err();
    b.rollback_transaction().unwrap();
    assert_eq!(a.get(b"right").unwrap().as_deref(), Some(&b"batch"[..]));
    assert_eq!(a.get(b"left").unwrap(), None);
}

#[test]
fn failed_or_undone_batches_leave_neither_records_nor_future_write_intents() {
    for failure in [
        "evaluation",
        "savepoint",
        "budget",
        "rejected checkpoint",
        "lost reply",
    ] {
        let persistence = Persistence::new();
        let (a, ra) = start(&persistence, false);
        let (b, rb) = start(&persistence, false);
        read(&ra, b"left");
        if failure == "savepoint" {
            a.savepoint("s").unwrap();
            batch_write(&a, b"right");
            a.rollback_to_savepoint("s").unwrap();
        } else {
            if failure == "rejected checkpoint" {
                persistence.checkpoint_fault(1, CheckpointFault::Reject);
            } else if failure == "lost reply" {
                persistence.checkpoint_fault(1, CheckpointFault::LoseReply);
            }
            let huge = vec![b'x'; 600_000];
            a.with_mutation(&mut |_, batch| {
                batch.observe_serializable_write(predicate(b"right"))?;
                batch.put(b"right", b"discard")?;
                if failure == "evaluation" {
                    return Err(StorageBackendError::Other(
                        "injected evaluation failure".into(),
                    ));
                }
                if failure == "budget" {
                    batch.observe_serializable_write(predicate(&huge))?;
                }
                Ok(())
            })
            .unwrap_err();
        }
        assert_eq!(a.get(b"right").unwrap(), None, "{failure}");
        // Register after undo: any surviving intent would close this cycle.
        read(&rb, b"right");
        batch_write(&b, b"left");
        batch_write(&a, b"independent");
        a.commit_transaction().unwrap();
        b.commit_transaction().unwrap();
        assert_eq!(a.get(b"right").unwrap(), None, "{failure}");
    }
}

#[test]
fn a_retained_batch_cannot_publish_under_another_serializable_participant() {
    let persistence = Persistence::new();
    let (session, original) = start(&persistence, false);
    let mut batch = session.batch();
    assert_eq!(batch.serializable_participant(), Some(original.id()));
    batch.observe_serializable_write(predicate(b"old")).unwrap();
    batch.put(b"old", b"discard").unwrap();
    session.rollback_transaction().unwrap();
    session.begin_transaction().unwrap();
    let next = session.establish_serializable_snapshot().unwrap();
    assert_ne!(original.id(), next.id());
    let error = batch.commit().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("changed serializable participant"),
        "{error}"
    );
    assert_eq!(session.get(b"old").unwrap(), None);
    batch_write(&session, b"current");
    session.commit_transaction().unwrap();
}

#[test]
fn graph_payload_addresses_distinguish_kinds_scopes_and_clear_generations() {
    use uqa_storage::catalog::{
        graph_identifiers::GraphIdentifierNamespace, graph_observations::GraphEntityKey,
    };
    use uqa_storage::GraphEntityKind::{Edge, Vertex};
    let original = GraphEntityKey::new(GraphIdentifierNamespace::new(None, [0; 16]), Vertex, 1);
    for (scope, generation, kind, id, conflict) in [
        (None, [0; 16], Vertex, 1, true),
        (None, [0; 16], Edge, 1, false),
        (Some(""), [0; 16], Vertex, 1, false),
        (Some("other"), [0; 16], Vertex, 1, false),
        (None, [1; 16], Vertex, 1, false),
        (None, [0; 16], Vertex, 2, false),
    ] {
        let persistence = Persistence::new();
        let (a, ra) = start(&persistence, false);
        let (b, rb) = start(&persistence, false);
        ra.observe_read(
            original.predicate(),
            &StorageReadControl::with_limit(1 << 20),
        )
        .unwrap();
        read(&rb, b"right");
        batch_write(&a, b"right");
        let key = GraphEntityKey::new(GraphIdentifierNamespace::new(scope, generation), kind, id);
        b.with_mutation(&mut |_, batch| {
            key.observe_write(batch)?;
            batch.put(b"graph row", b"changed")
        })
        .unwrap();
        a.commit_transaction().unwrap();
        if conflict {
            b.commit_transaction().unwrap_err();
            b.rollback_transaction().unwrap();
        } else {
            b.commit_transaction().unwrap();
        }
    }
}
