//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn relation(oid: u32) -> PreparedAnalysisDependencies {
    PreparedAnalysisDependencies {
        relations: BTreeSet::from([oid]),
        ..Default::default()
    }
}

#[test]
fn catalog_events_match_only_selected_objects_until_global_invalidation() {
    let dependencies = PreparedAnalysisDependencies {
        relations: BTreeSet::from([10]),
        routines: BTreeSet::from([[7; 16]]),
    };
    assert!(PreparedCatalogChange::Relation(10).affects(&dependencies));
    assert!(!PreparedCatalogChange::Relation(11).affects(&dependencies));
    assert!(PreparedCatalogChange::Routine([7; 16]).affects(&dependencies));
    assert!(!PreparedCatalogChange::Routine([8; 16]).affects(&dependencies));
    let mut log = PreparedInvalidationLog::default();
    assert!(!log.affects(&dependencies));
    log.record(PreparedCatalogChange::Relation(11));
    assert!(!log.affects(&dependencies));
    log.record(PreparedCatalogChange::GlobalCatalog);
    assert!(log.affects(&dependencies));
    assert!(log.affects(&PreparedAnalysisDependencies::default()));
    log.record(PreparedCatalogChange::Routine([8; 16]));
    assert_eq!(log.levels[0].len(), 1);
}

#[test]
fn rollback_replays_undone_changes_without_retaining_them_for_outer_commit() {
    let mut log = PreparedInvalidationLog::default();
    log.record(PreparedCatalogChange::Relation(10));
    let mark = log.mark();
    log.record(PreparedCatalogChange::Relation(10));
    log.record(PreparedCatalogChange::Relation(20));
    let nested = log.mark();
    log.record(PreparedCatalogChange::Relation(30));
    log.release(nested);
    let undone = log.rollback_to(mark);
    for oid in [10, 20, 30] {
        assert!(undone.affects(&relation(oid)));
    }
    assert!(log.affects(&relation(10)));
    assert!(!log.affects(&relation(20)));
    assert!(!log.affects(&relation(30)));
    assert!(!log.rollback_to(mark).affects(&relation(10)));
    log.record(PreparedCatalogChange::Relation(40));
    log.release(mark);
    assert!(log.affects(&relation(40)));
}

#[test]
fn nested_transaction_commit_merges_events_into_the_current_parent_savepoint() {
    let mut parent = PreparedInvalidationLog::default();
    parent.record(PreparedCatalogChange::Relation(10));
    let mark = parent.mark();
    let mut child = PreparedInvalidationLog::default();
    child.record(PreparedCatalogChange::Relation(20));
    let _child_mark = child.mark();
    child.record(PreparedCatalogChange::GlobalCatalog);
    parent.append(child);
    assert!(parent.affects(&relation(30)));
    assert!(parent.rollback_to(mark).affects(&relation(30)));
    assert!(parent.affects(&relation(10)));
    assert!(!parent.affects(&relation(20)));
}
