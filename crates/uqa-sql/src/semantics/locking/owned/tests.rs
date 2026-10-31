//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{LockStrength, LockWait},
    plan::UnifiedPlan,
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn target() -> ResolvedRowLock {
    ResolvedRowLock {
        qualifier: "v".into(),
        storage_name: "base".into(),
        display_name: "v".into(),
        strength: LockStrength::ForUpdate,
        wait: LockWait::NoWait,
        identity_source: false,
    }
}

#[test]
fn row_marks_follow_only_from_subqueries_and_keep_other_syntax() {
    for sql in [
        "SELECT v.x FROM (SELECT x FROM base FOR SHARE) v JOIN (SELECT x FROM other) o ON v.x=o.x",
        "WITH untouched AS (SELECT x FROM base) SELECT (SELECT x FROM scalar_source) FROM (SELECT x FROM base) v",
        "SELECT x FROM base UNION ALL SELECT x FROM other",
    ] {
        let UnifiedPlan::Query(mut expected) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0)) else { panic!("query") };
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let query = expected.copy_for_rewrite(&control).unwrap();
        crate::semantics::locking::apply_propagated_view_lock(&mut expected, &target());
        let actual = apply_propagated_view_lock_owned(query, &target(), &control).unwrap();
        assert_eq!(serde_json::to_value(&*actual).unwrap(), serde_json::to_value(&expected).unwrap(), "{sql}");
        drop(actual);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn row_mark_failure_releases_constructor_records_and_keeps_original_syntax() {
    for failure in 0..3 {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let UnifiedPlan::Query(source) = UnifiedPlan::lower(
            crate::compile("SELECT v.x FROM (SELECT x FROM base FOR SHARE) v")
                .unwrap()
                .remove(0),
        ) else {
            panic!("query")
        };
        let expected = serde_json::to_value(&source).unwrap();
        let query = source.copy_for_rewrite(&control).unwrap();
        let occupied = (failure == 0).then(|| budget.reserve(budget.available()).unwrap());
        if failure == 1 {
            original.cancel();
        }
        if failure == 2 {
            invoking.cancel();
        }
        let error = apply_propagated_view_lock_owned(query, &target(), &control).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if failure == 0 { "53200" } else { "57014" })
        );
        assert_eq!(serde_json::to_value(&source).unwrap(), expected);
        assert_eq!(
            budget.used(),
            occupied
                .as_ref()
                .map_or(0, uqa_core::memory::MemoryReservation::bytes)
        );
        drop(occupied);
        assert_eq!(budget.used(), 0);
    }
}
