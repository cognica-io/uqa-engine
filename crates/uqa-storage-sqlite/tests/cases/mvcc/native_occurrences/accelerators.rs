//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native accelerators remain paired with their source across conversion and competing writers.

use super::*;
use std::cell::{Cell, RefCell};
use uqa_storage::block_max_index::{BlockMaxIndex, BlockMaxScorer};

struct Frequency;
impl BlockMaxScorer for Frequency {
    fn score(&self, frequency: u64, _: u64, _: u64) -> f64 {
        frequency as f64
    }
}
struct Interleaved<'a> {
    effect: RefCell<Option<Box<dyn FnOnce() + 'a>>>,
    calls: Cell<usize>,
}
impl BlockMaxScorer for Interleaved<'_> {
    fn score(&self, frequency: u64, _: u64, _: u64) -> f64 {
        self.calls.set(self.calls.get() + 1);
        if let Some(effect) = self.effect.borrow_mut().take() {
            effect();
        }
        frequency as f64
    }
}

#[test]
fn native_accelerators_preserve_conversion_retained_branches_and_reopen() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("accelerators.db");
        let connection = open(mode, &path);
        Catalog::open(connection.clone()).unwrap();
        let mut live = index(&connection, "docs_é");
        live.try_add_documents(
            (0..129)
                .map(|id| {
                    (
                        id * 3,
                        fields(if id == 128 {
                            "alpha alpha alpha"
                        } else {
                            "alpha alpha"
                        }),
                    )
                })
                .collect(),
        )
        .unwrap();
        live.flush_skip_pointers().unwrap();
        live.rebuild_persisted_block_max("body", &Frequency, "frequency")
            .unwrap();
        bind(&connection);
        assert_eq!(live.skip_to("body", "alpha", 384).unwrap(), (384, 128));
        assert_eq!(live.get_block_max_score("body", "alpha", 1).unwrap(), 3.0);
        assert_eq!(
            live.get_all_block_max_scores("body", "alpha").unwrap(),
            [2.0, 3.0]
        );
        let expected = Some(vec![2.0, 3.0]);
        assert_eq!(
            live.persisted_block_max_scores("body", "alpha", "frequency")
                .unwrap(),
            expected
        );
        let retained = live.snapshot().unwrap();
        connection.begin_transaction().unwrap();
        connection.savepoint("original").unwrap();
        live.add_document(0, fields("alpha alpha alpha alpha"))
            .unwrap();
        assert_eq!(
            live.persisted_block_max_scores("body", "alpha", "frequency")
                .unwrap(),
            None
        );
        assert_eq!(live.skip_to("body", "alpha", 384).unwrap(), (0, 0));
        live.rebuild_persisted_block_max("body", &Frequency, "private")
            .unwrap();
        let discarded = live.snapshot().unwrap();
        connection.rollback_to_savepoint("original").unwrap();
        connection.commit_transaction().unwrap();
        assert_eq!(
            discarded
                .persisted_block_max_scores("body", "alpha", "private")
                .unwrap(),
            Some(vec![4.0, 3.0])
        );
        assert_eq!(
            retained
                .persisted_block_max_scores("body", "alpha", "frequency")
                .unwrap(),
            expected
        );
        live.remove_document(0).unwrap();
        live.flush_skip_pointers().unwrap();
        live.rebuild_persisted_block_max("body", &Frequency, "current")
            .unwrap();
        assert_eq!(live.skip_to("body", "alpha", 384).unwrap(), (3, 0));
        let mut loaded = BlockMaxIndex::default();
        live.load_block_max_into(&mut loaded).unwrap();
        assert_eq!(
            loaded.block_maxes("docs_é", "body", "alpha"),
            Some([3.0].as_slice())
        );
        drop((connection, live, retained, discarded));
        let connection = open(mode, &path);
        bind(&connection);
        let live = index(&connection, "docs_é");
        assert_eq!(live.get_doc_length(384, "body").unwrap(), 3);
        assert_eq!(
            live.persisted_block_max_scores("body", "alpha", "current")
                .unwrap(),
            Some(vec![3.0])
        );
        assert_eq!(live.skip_to("body", "alpha", 384).unwrap(), (3, 0));
    }
}

#[test]
fn native_accelerator_builds_never_replay_or_publish_after_their_source_changes() {
    let connection = memory();
    let other = connection.new_session();
    let mut live = index(&connection, "docs");
    live.try_add_documents((0..129).map(|id| (id, fields("alpha alpha"))).collect())
        .unwrap();
    let scorer = Interleaved {
        effect: RefCell::new(Some(Box::new(|| {
            index(&other, "docs")
                .add_document(0, fields("alpha alpha alpha"))
                .unwrap();
        }))),
        calls: Cell::new(0),
    };
    connection.begin_transaction().unwrap();
    live.rebuild_persisted_block_max("body", &scorer, "stale")
        .unwrap();
    assert_eq!(scorer.calls.get(), 129);
    assert_eq!(
        live.persisted_block_max_scores("body", "alpha", "stale")
            .unwrap(),
        Some(vec![2.0, 2.0])
    );
    assert!(connection.commit_transaction().is_err());
    assert_eq!(scorer.calls.get(), 129);
    connection.rollback_transaction().unwrap();
    assert_eq!(
        live.persisted_block_max_scores("body", "alpha", "stale")
            .unwrap(),
        None
    );
    assert_eq!(live.get_doc_length(0, "body").unwrap(), 3);

    connection.begin_transaction().unwrap();
    live.add_document(0, fields("alpha")).unwrap();
    index(&other, "docs")
        .rebuild_persisted_block_max("body", &Frequency, "winner")
        .unwrap();
    assert!(connection.commit_transaction().is_err());
    connection.rollback_transaction().unwrap();
    assert_eq!(
        live.persisted_block_max_scores("body", "alpha", "winner")
            .unwrap(),
        Some(vec![3.0, 2.0])
    );
}

#[test]
fn native_column_changes_invalidate_bounds_and_fence_late_builds() {
    let connection = memory();
    let catalog = Catalog::open(connection.clone()).unwrap();
    let other = connection.new_session();
    let mut live = index(&connection, "docs");
    live.add_document(1, fields("alpha alpha")).unwrap();
    connection.begin_transaction().unwrap();
    catalog
        .rename_column_data("docs", "body", "renamed")
        .unwrap();
    index(&other, "docs")
        .rebuild_persisted_block_max("body", &Frequency, "late")
        .unwrap();
    assert!(connection.commit_transaction().is_err());
    connection.rollback_transaction().unwrap();
    let retained = live.snapshot().unwrap();
    catalog
        .rename_column_data("docs", "body", "renamed")
        .unwrap();
    for field in ["body", "renamed"] {
        assert_eq!(
            live.persisted_block_max_scores(field, "alpha", "late")
                .unwrap(),
            None
        );
    }
    assert_eq!(live.get_doc_length(1, "renamed").unwrap(), 2);
    assert_eq!(
        retained
            .persisted_block_max_scores("body", "alpha", "late")
            .unwrap(),
        Some(vec![2.0])
    );
    live.rebuild_persisted_block_max("renamed", &Frequency, "new")
        .unwrap();
    catalog.drop_column_data("docs", "renamed").unwrap();
    assert_eq!(
        live.persisted_block_max_scores("renamed", "alpha", "new")
            .unwrap(),
        None
    );
}
