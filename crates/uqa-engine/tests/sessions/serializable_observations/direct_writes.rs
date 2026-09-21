//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed document mutations publish row intents independently of index changes.

use super::{fixtures, Session};
use std::collections::BTreeMap;
use uqa_core::{DocId, Value};
use uqa_storage::document_store::Document;

#[derive(Clone, Copy, Debug)]
enum Mutation {
    Replace,
    ReplaceWithVectors,
    Update,
    Patch,
    Delete,
}

impl Mutation {
    const ALL: [Self; 5] = [
        Self::Replace,
        Self::ReplaceWithVectors,
        Self::Update,
        Self::Patch,
        Self::Delete,
    ];

    fn apply(self, session: &Session, id: DocId) {
        let engine = &session.engine;
        let fields = BTreeMap::from([("v".into(), Value::Int(2))]);
        let document = || {
            Document::from([
                ("id".into(), Value::Int(i64::try_from(id).unwrap())),
                ("v".into(), Value::Int(2)),
            ])
        };
        match self {
            Self::Replace => engine.add_document("left_t", id, document()).unwrap(),
            Self::ReplaceWithVectors => engine
                .add_document_with_vector_values("left_t", id, document(), BTreeMap::new())
                .unwrap(),
            Self::Update => assert!(engine
                .update_document_fields_with_vector_values("left_t", id, fields, BTreeMap::new())
                .unwrap()),
            Self::Patch => assert!(engine
                .patch_document_fields_with_vector_values("left_t", id, &fields, &BTreeMap::new())
                .unwrap()),
            Self::Delete => engine.delete_document("left_t", id).unwrap(),
        }
    }

    fn apply_to_missing(self, session: &Session, id: DocId) {
        let engine = &session.engine;
        let fields = BTreeMap::from([("v".into(), Value::Int(2))]);
        match self {
            Self::Update => assert!(!engine
                .update_document_fields_with_vector_values("left_t", id, fields, BTreeMap::new())
                .unwrap()),
            Self::Patch => assert!(!engine
                .patch_document_fields_with_vector_values("left_t", id, &fields, &BTreeMap::new())
                .unwrap()),
            Self::Delete => engine.delete_document("left_t", id).unwrap(),
            _ => unreachable!("this mutation creates an absent target"),
        }
    }
}

fn crossed_reads(a: &Session, b: &Session, id: DocId) {
    a.sql("BEGIN ISOLATION LEVEL SERIALIZABLE");
    b.sql("BEGIN ISOLATION LEVEL SERIALIZABLE");
    assert_eq!(
        a.engine.get_document("left_t", id).unwrap().unwrap()["v"],
        Value::Int(1)
    );
    b.sql("SELECT v FROM right_t");
    a.sql("UPDATE right_t SET v = 2 WHERE id = 1");
}

#[test]
fn direct_replacements_updates_and_deletes_conflict_with_point_reads_in_both_commit_orders() {
    for mutation in Mutation::ALL {
        for reverse in [false, true] {
            let (_directory, sessions) = fixtures();
            for (provider, a) in sessions.into_iter().enumerate() {
                let b = a.sibling();
                let id = a.engine.table_doc_ids("left_t").unwrap()[0];
                crossed_reads(&a, &b, id);
                mutation.apply(&b, id);
                let (winner, loser) = if reverse { (&b, &a) } else { (&a, &b) };
                winner.sql("COMMIT");
                let result = loser.engine.sql("COMMIT", &[]);
                assert!(
                    result.is_err(),
                    "expected a serialization cycle: {mutation:?}, reverse={reverse}, provider={provider}: {result:?}"
                );
                assert_eq!(result.unwrap_err().sqlstate(), Some("40001"));
                assert_eq!(loser.engine.transaction_depth(), 0);
            }
        }
    }
}

#[test]
fn absent_direct_updates_and_deletes_do_not_manufacture_row_writes() {
    for mutation in [Mutation::Update, Mutation::Patch, Mutation::Delete] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            let b = a.sibling();
            a.sql("BEGIN ISOLATION LEVEL SERIALIZABLE");
            b.sql("BEGIN ISOLATION LEVEL SERIALIZABLE");
            assert!(a.engine.get_document("left_t", 99).unwrap().is_none());
            b.sql("SELECT v FROM right_t");
            a.sql("UPDATE right_t SET v = 2 WHERE id = 1");
            mutation.apply_to_missing(&b, 99);
            a.sql("COMMIT");
            b.sql("COMMIT");
        }
    }
}

fn insert_and_commit(session: &Session, id: DocId) -> Result<(), uqa_sql::SQLError> {
    session.engine.add_document(
        "left_t",
        id,
        Document::from([
            ("id".into(), Value::Int(i64::try_from(id).unwrap())),
            ("v".into(), Value::Int(2)),
        ]),
    )?;
    session.engine.sql("COMMIT", &[]).map(|_| ())
}

#[test]
fn absent_direct_mutation_reads_track_only_the_selected_row_across_savepoint_undo() {
    for mutation in [Mutation::Update, Mutation::Patch, Mutation::Delete] {
        for undo in [false, true] {
            for inserted in [99, 100] {
                let (_directory, sessions) = fixtures();
                for (provider, a) in sessions.into_iter().enumerate() {
                    let b = a.sibling();
                    a.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SAVEPOINT before_probe");
                    b.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT v FROM right_t");
                    mutation.apply_to_missing(&a, 99);
                    if undo {
                        a.sql("ROLLBACK TO before_probe");
                    }
                    a.sql("RELEASE before_probe; UPDATE right_t SET v = 2 WHERE id = 1");
                    // Finish the reader before requesting its retained tuple lock for the insertion.
                    a.sql("COMMIT");
                    let result = insert_and_commit(&b, inserted);
                    assert_eq!(
                        result.is_err(),
                        inserted == 99,
                        "{mutation:?}, undo={undo}, inserted={inserted}, provider={provider}: {result:?}"
                    );
                    if inserted == 99 {
                        assert_eq!(result.unwrap_err().sqlstate(), Some("40001"));
                    }
                    if b.engine.transaction_depth() != 0 {
                        b.sql("ROLLBACK");
                    }
                    assert_eq!(
                        a.engine.get_document("left_t", inserted).unwrap().is_some(),
                        inserted != 99
                    );
                }
            }
        }
    }
}

#[test]
fn whole_transaction_rollback_discards_direct_target_read_dependencies() {
    for mutation in [Mutation::Update, Mutation::Patch, Mutation::Delete] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            let b = a.sibling();
            a.sql("BEGIN ISOLATION LEVEL SERIALIZABLE");
            b.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT v FROM right_t");
            mutation.apply_to_missing(&a, 99);
            a.sql("UPDATE right_t SET v = 2 WHERE id = 1; ROLLBACK");
            insert_and_commit(&b, 99).unwrap();
            assert!(a.engine.get_document("left_t", 99).unwrap().is_some());
            assert_eq!(a.sql("SELECT v FROM right_t").rows[0]["v"], Value::Int(1));
        }
    }
}

#[test]
fn savepoint_undo_removes_direct_row_intents_but_keeps_observed_dependencies() {
    for mutation in Mutation::ALL {
        for observed_before_undo in [false, true] {
            let (_directory, sessions) = fixtures();
            for (provider, a) in sessions.into_iter().enumerate() {
                let b = a.sibling();
                let id = a.engine.table_doc_ids("left_t").unwrap()[0];
                a.begin();
                b.begin();
                if observed_before_undo {
                    assert!(a.engine.get_document("left_t", id).unwrap().is_some());
                }
                b.sql("SELECT v FROM right_t");
                b.sql("SAVEPOINT before_write");
                mutation.apply(&b, id);
                b.sql("ROLLBACK TO before_write; RELEASE before_write");
                // A later point read must not encounter the cancelled intent; an already observed dependency survives undo.
                assert_eq!(
                    a.engine.get_document("left_t", id).unwrap().unwrap()["v"],
                    Value::Int(1)
                );
                a.sql("UPDATE right_t SET v = 2 WHERE id = 1");
                a.sql("COMMIT");
                let result = b.engine.sql("COMMIT", &[]);
                assert_eq!(
                    result.is_err(),
                    observed_before_undo,
                    "{mutation:?}, observed_before_undo={observed_before_undo}, provider={provider}: {result:?}"
                );
                if observed_before_undo {
                    assert_eq!(result.unwrap_err().sqlstate(), Some("40001"));
                }
                assert_eq!(b.engine.transaction_depth(), 0);
                assert_eq!(
                    b.engine.get_document("left_t", id).unwrap().unwrap()["v"],
                    Value::Int(1),
                    "{mutation:?}"
                );
            }
        }
    }
}
