//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_execution::operator_tree::driver::context::RetrievalIndexes;

const CREATE: &str = "CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)";
const BEFORE: &[(i64, f64)] = &[(1, 1.0), (2, 0.0), (4, 0.0), (3, -1.0)];
const AFTER: &[(i64, f64)] = &[(5, 1.0), (4, 0.0), (1, -1.0), (3, -1.0)];

#[derive(Clone, Copy, Debug)]
enum Action {
    Create,
    Rebuild,
    Drop,
}

fn kind(engine: &Engine) -> String {
    engine
        .table_indexes("diskann_docs")
        .unwrap()
        .unwrap()
        .vector_indexes()
        .get("embedding")
        .unwrap()
        .index_kind()
        .to_owned()
}

fn verify(engine: &Engine, expected_kind: &str, changed: bool) {
    let result = engine.sql("SELECT id, _score FROM diskann_docs WHERE knn_match(embedding, ARRAY[1.0,0.0], 10) ORDER BY _score DESC, id", &[]).unwrap();
    let expected = if changed { AFTER } else { BEFORE };
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| {
                let Value::Int(id) = row["id"] else {
                    panic!("integer identity")
                };
                let Value::Float(score) = row["_score"] else {
                    panic!("raw cosine score")
                };
                (id, score.to_bits())
            })
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|&(id, score)| (id, score.to_bits()))
            .collect::<Vec<_>>()
    );
    assert_eq!(kind(engine), expected_kind);
    assert_eq!(
        engine.catalog_index("diskann_idx").unwrap().is_some(),
        expected_kind == "diskann"
    );
    let rows = engine
        .sql("SELECT id FROM diskann_docs ORDER BY id", &[])
        .unwrap();
    assert_eq!(
        rows.rows
            .iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        if changed { [1, 3, 4, 5] } else { [1, 2, 3, 4] }.map(Value::Int)
    );
}

/// Check the presentations of the pending batch during its resolution, which `publishes` when the resolution commits a batch the provider never committed. Each keeps the original transaction and fingerprint. A commit after the lost reply, by the independent writer or by background journal pruning, makes the provider reject the snapshot the batch's derived effects were resolved at; storage then resolves those effects again and presents the same batch once more. Re-presenting the original batch may also read its already committed receipt inside the provider.
fn assert_presentations(
    presentations: &[Presentation],
    evaluated: Presentation,
    publishes: bool,
    intervening: bool,
) {
    for presentation in presentations {
        assert_eq!(
            (presentation.transaction, presentation.fingerprint),
            (evaluated.transaction, evaluated.fingerprint),
            "any re-presented batch retains its original transaction and fingerprint"
        );
    }
    let rejected = presentations
        .iter()
        .filter(|presentation| presentation.snapshot_changed)
        .count();
    assert!(
        presentations.len() - rejected <= 1,
        "resolution cannot reconstruct a generation"
    );
    if publishes {
        assert_eq!(
            presentations.len() - rejected,
            1,
            "the original pending batch is committed"
        );
        assert!(
            !intervening || rejected > 0,
            "the intervening commit outdates the derived snapshot"
        );
    }
}

/// Commit a row of another table from an independent session on another thread, whose commit the fault double neither counts as the foreground's nor faults.
fn commit_independently(root: &Engine) {
    let writer = root.new_session().unwrap();
    std::thread::spawn(move || {
        writer
            .sql("INSERT INTO diskann_side VALUES (1)", &[])
            .unwrap()
    })
    .join()
    .unwrap();
}

/// Run `action` in a transaction whose commit loses its reply through `fault`, then resolve it by COMMIT or ROLLBACK. With `intervening`, an independent session commits between the lost reply and the resolution, so the snapshot the pending batch's derived effects were resolved at is no longer current.
fn exercise(
    persistence: &Arc<FaultPersistence>,
    action: Action,
    fault: u8,
    rollback: bool,
    intervening: bool,
) {
    *persistence.attempt.lock().unwrap() = None;
    let root = engine(persistence.clone());
    root.sql("CREATE TABLE diskann_docs(id int, embedding tensor(2)); INSERT INTO diskann_docs VALUES (1,ARRAY[ARRAY[1.0,0.0]]),(2,ARRAY[ARRAY[0.0,1.0],ARRAY[-1.0,0.0]]),(3,ARRAY[ARRAY[-1.0,0.0]]),(4,ARRAY[ARRAY[0.0,0.0]]); CREATE TABLE diskann_side(id int)", &[]).unwrap();
    let exact = kind(&root);
    if !matches!(action, Action::Create) {
        root.sql(CREATE, &[]).unwrap();
    }
    let original_kind = kind(&root);
    let peer = root.new_session().unwrap();
    verify(&peer, &original_kind, false);
    root.sql("BEGIN; UPDATE diskann_docs SET embedding=ARRAY[ARRAY[-1.0,0.0]] WHERE id=1; DELETE FROM diskann_docs WHERE id=2; INSERT INTO diskann_docs VALUES (5,ARRAY[ARRAY[0.0,1.0],ARRAY[1.0,0.0]])", &[]).unwrap();
    root.sql(
        match action {
            Action::Create => CREATE,
            Action::Rebuild => "ALTER TABLE diskann_docs ALTER COLUMN embedding TYPE tensor(2)",
            Action::Drop => "DROP INDEX diskann_idx",
        },
        &[],
    )
    .unwrap_or_else(|error| {
        panic!("DiskANN DDL {action:?}, fault {fault}, rollback {rollback}: {error}")
    });
    let published_kind = if matches!(action, Action::Drop) {
        exact.as_str()
    } else {
        "diskann"
    };
    verify(&root, published_kind, true);

    persistence.fault.store(fault, Ordering::Release);
    assert_unknown(&root.commit().unwrap_err());
    let pending = root.pending_commit().unwrap();
    let transaction = persistence.attempt.lock().unwrap().unwrap();
    let evaluated = persistence.last_foreground_presentation().unwrap();
    assert_eq!(evaluated.transaction, transaction);
    assert_unknown(&root.sql("SELECT * FROM diskann_docs", &[]).unwrap_err());
    assert_eq!(root.pending_commit(), Some(pending));
    let durable = fault == LOSE_COMMITTED_REPLY;
    let observed = persistence
        .inner
        .commit_status(transaction, &StorageReadControl::with_limit(1 << 20))
        .unwrap();
    assert_eq!(matches!(observed, CommitStatus::Committed(_)), durable);
    if intervening {
        commit_independently(&root);
    }
    let writes = persistence.foreground_record_writes();
    persistence.fault.store(HEALTHY, Ordering::Release);
    if rollback {
        if durable {
            let error = root.rollback().unwrap_err();
            assert_eq!(error.sqlstate(), Some("25000"));
            assert!(error.to_string().contains("already committed"));
        } else {
            root.rollback().unwrap();
        }
    } else {
        root.commit().unwrap();
    }
    let resolved = persistence.foreground_record_writes();
    assert_eq!(
        resolved.0, writes.0,
        "resolution cannot allocate a new staging transaction"
    );
    assert_presentations(
        &persistence.foreground_presentations_after(writes.1),
        evaluated,
        !durable && !rollback,
        intervening,
    );
    assert!(root.pending_commit().is_none());
    assert_eq!(root.transaction_depth(), 0);
    let committed = durable || !rollback;
    let status = persistence
        .inner
        .commit_status(transaction, &StorageReadControl::with_limit(1 << 20))
        .unwrap();
    assert_eq!(matches!(status, CommitStatus::Committed(_)), committed);
    assert_eq!(matches!(status, CommitStatus::Aborted), !committed);
    if durable {
        assert_eq!(status, observed, "confirmed receipt remains unchanged");
    }
    let expected_kind = if committed {
        published_kind
    } else {
        &original_kind
    };
    verify(&root, expected_kind, committed);
    verify(&peer, expected_kind, committed);
    let reopened = engine(persistence.clone());
    verify(&reopened, expected_kind, committed);
    if intervening {
        assert_eq!(count(&reopened, "diskann_side"), Value::Int(1));
    }
    drop((reopened, peer));
    root.sql("DROP TABLE diskann_docs; DROP TABLE diskann_side", &[])
        .unwrap();
}

#[rstest::rstest]
#[case::plain("plain")]
#[case::encrypted("encrypted")]
#[case::compressed("compressed")]
#[case::compressed_encrypted("compressed-encrypted")]
#[case::redb("redb")]
fn diskann_publication_receipt_recovery_preserves_catalog_queries_and_original_attempts(
    #[case] provider: &str,
    #[values(Action::Create, Action::Rebuild, Action::Drop)] action: Action,
    #[values(LOSE_COMMITTED_REPLY, LOSE_UNCOMMITTED_REPLY)] fault: u8,
    #[values(false, true)] rollback: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let persistence = fixture(directory.path(), provider);
    exercise(&persistence, action, fault, rollback, false);
}

#[rstest::rstest]
#[case::plain("plain")]
#[case::encrypted("encrypted")]
#[case::compressed("compressed")]
#[case::compressed_encrypted("compressed-encrypted")]
#[case::redb("redb")]
fn diskann_publication_resolution_resolves_derived_effects_again_after_an_intervening_commit(
    #[case] provider: &str,
    #[values(Action::Create, Action::Rebuild, Action::Drop)] action: Action,
) {
    let directory = tempfile::tempdir().unwrap();
    let persistence = fixture(directory.path(), provider);
    exercise(&persistence, action, LOSE_UNCOMMITTED_REPLY, false, true);
}
