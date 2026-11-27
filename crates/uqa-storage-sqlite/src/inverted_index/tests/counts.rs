//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! GIN corpus counts use one selected key scan, preserving multicolumn and private membership.

use super::*;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_storage::mvcc::VersionedSessionOptions;

#[test]
fn bulk_postings_read_source_metadata_in_batches_and_share_the_field_revision() {
    let mut index = idx_with_analyzer(uqa_analysis::whitespace_analyzer());
    let ids: Vec<_> = (1..=128).chain([65_536, 131_072]).collect();
    index
        .try_add_documents(
            ids.iter()
                .map(|&id| (id, fields([("body", "word word")])))
                .collect(),
        )
        .unwrap();
    let documents = Arc::new(AtomicUsize::new(0));
    let revisions = Arc::new(AtomicUsize::new(0));
    let (document_reads, revision_reads) = (Arc::clone(&documents), Arc::clone(&revisions));
    index
        .conn
        .with_physical(|sqlite| {
            sqlite.set_prepared_statement_cache_capacity(0);
            sqlite.authorizer(Some(move |context: AuthContext<'_>| {
                match context.action {
                    AuthAction::Read {
                        table_name: "_occurrence_documents",
                        column_name: "metadata_blob",
                    } => {
                        document_reads.fetch_add(1, Ordering::Relaxed);
                    }
                    AuthAction::Read {
                        table_name: "_occurrence_fields",
                        column_name: "revision",
                    } => {
                        revision_reads.fetch_add(1, Ordering::Relaxed);
                    }
                    _ => {}
                }
                Authorization::Allow
            }))?;
            Ok(())
        })
        .unwrap();
    let terms = ["word", "absent", "word"].map(TokenTermKey::from_text);
    let postings = index.occurrence_postings_bulk("body", &terms).unwrap();
    assert!(postings[1].is_empty());
    assert_eq!(postings[0], postings[2]);
    assert_eq!(
        postings[0].iter().map(|p| p.doc_id).collect::<Vec<_>>(),
        ids
    );
    assert!(postings[0].iter().all(|p| p.occurrences.len() == 2));
    assert_eq!(
        documents.load(Ordering::Relaxed),
        3,
        "three posting clusters"
    );
    assert_eq!(revisions.load(Ordering::Relaxed), 1, "one field revision");
}

#[test]
fn bulk_postings_report_missing_source_in_document_order() {
    let mut index = idx_with_analyzer(uqa_analysis::whitespace_analyzer());
    for id in 1..=3 {
        index.add_document(id, fields([("body", "word")])).unwrap();
    }
    index
        .conn
        .with(|sqlite| {
            sqlite.execute_batch(
                "DELETE FROM _occurrence_documents WHERE doc_id = 1;
            UPDATE _occurrence_documents SET metadata_blob = x'00' WHERE doc_id = 2;
            DELETE FROM _occurrence_fields;",
            )?;
            Ok(())
        })
        .unwrap();
    let error = index
        .occurrence_postings_bulk("body", &[TokenTermKey::from_text("word")])
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("occurrence source metadata is missing"),
        "{error}"
    );
}

#[test]
fn native_document_count_workspace_does_not_grow_with_the_corpus() {
    for count in [32, 128] {
        let mut index = idx_with_analyzer(uqa_analysis::whitespace_analyzer());
        index
            .conn
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        index.conn.begin_transaction().unwrap();
        for id in 1..=count {
            // Native text order and length-prefixed occurrence order disagree.
            index
                .add_document(id, fields([("a_long_name", ""), ("z", "")]))
                .unwrap();
        }
        index.conn.commit_transaction().unwrap();
        let retained = index.snapshot().unwrap();
        let control = index.conn.retention_control().unwrap();
        let available = 16 * 1024;
        let occupied = control
            .memory()
            .reserve(control.memory().limit() - control.memory().used() - available)
            .unwrap();
        assert_eq!(
            retained.doc_count().unwrap(),
            count,
            "{count} documents must fit the same {available}-byte workspace"
        );
        drop(occupied);
        assert_eq!(index.doc_count().unwrap(), count);
    }
}

#[test]
fn native_document_counts_do_not_fetch_each_field_length() {
    for count in [32, 128] {
        let mut index = idx_with_analyzer(uqa_analysis::whitespace_analyzer());
        index
            .conn
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        index.conn.begin_transaction().unwrap();
        for id in 1..=count {
            index
                .add_document(
                    id,
                    fields([
                        ("body", if id % 2 == 0 { "alpha beta" } else { "" }),
                        ("long_title", "gamma"),
                    ]),
                )
                .unwrap();
        }
        index.conn.commit_transaction().unwrap();
        let snapshot = index.snapshot().unwrap();
        let selects = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&selects);
        index
            .conn
            .with_physical(|sqlite| {
                sqlite.set_prepared_statement_cache_capacity(0);
                sqlite.authorizer(Some(move |context: AuthContext<'_>| {
                    if matches!(context.action, AuthAction::Select) {
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                    Authorization::Allow
                }))?;
                Ok(())
            })
            .unwrap();
        assert_eq!(snapshot.doc_count().unwrap(), count);
        let observed = selects.load(Ordering::Relaxed);
        index
            .conn
            .with_physical(|sqlite| {
                sqlite.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
                sqlite.set_prepared_statement_cache_capacity(
                    crate::connection::PREPARED_STATEMENT_CACHE_CAPACITY,
                );
                Ok(())
            })
            .unwrap();
        assert!(
            (1..=64).contains(&observed),
            "{count} documents required {observed} SELECTs"
        );
        index.conn.begin_transaction().unwrap();
        index.remove_document(1).unwrap();
        let private = index.snapshot().unwrap();
        assert_eq!(private.doc_count().unwrap(), count - 1);
        assert_eq!(snapshot.doc_count().unwrap(), count);
        index.conn.rollback_transaction().unwrap();
        assert_eq!(index.doc_count().unwrap(), count);
        assert_eq!(private.doc_count().unwrap(), count - 1);
    }
}
