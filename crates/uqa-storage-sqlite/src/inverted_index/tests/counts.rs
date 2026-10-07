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
