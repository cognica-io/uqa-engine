//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::subqueries::test_support::{plan, Services};
use crate::{CanonicalRowHashSet, PhysicalRow, RowPredicate, RowSchema};

#[test]
fn prepared_predicate_retains_its_scope_and_borrows_hooks_for_each_row() {
    let services = Services::default();
    let mut scope = CteScope::new();
    scope.scalar_subqueries.push(plan("SELECT 7"));
    let mut keys = CanonicalRowHashSet::new();
    keys.insert_values(&[Value::Int(7)]).unwrap();
    let lookup = Arc::new(CachedCorrelatedExists {
        outer_keys: CorrelatedExistsOuterKeys::Evaluated(vec![ScalarExpr::ScalarSubquery(0)]),
        keys,
    });
    let mut predicate = PreparedCorrelatedExistsPredicate {
        hooks: &services,
        params: &[],
        ctes: scope.clone(),
        lookup,
        negated: false,
    };
    scope.scalar_subqueries.clear();
    let schema = RowSchema::new(vec![]);
    let row = PhysicalRow::from_values(vec![]);
    assert!(predicate.keep_physical(&schema, &row).unwrap());
    predicate.negated = true;
    assert!(!predicate.keep_physical(&schema, &row).unwrap());
    assert_eq!(
        *services.events.lock(),
        ["hooks", "nested", "hooks", "nested"]
    );
}
