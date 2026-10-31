//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, RelationIdentity};

#[test]
fn lexical_lock_scopes_borrow_names_and_admit_only_new_nodes() {
    let name = "x".repeat(4 << 20);
    let budget = MemoryBudget::new(128 << 10);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let empty = BindingNameSet::new();
    let mut parent = LocalNames::new(&empty, control);
    let allocation = allocation_counter::measure(|| parent.insert(&name).unwrap());
    assert!(allocation.bytes_total < 1024, "{allocation:?}");
    assert_eq!(allocation.bytes_current, budget.used() as i64);
    let baseline = budget.used();
    let mut child = LocalNames::new(&parent, control);
    assert!(child.contains(RelationName::decoded(&name)));
    child.insert("a\"b").unwrap();
    let reference = RelationIdentity::borrowed_reference("\"a\"\"b\"").unwrap();
    assert!(child.contains(reference.name));
    assert!(!parent.contains(reference.name));
    let occupied = budget.reserve(budget.available()).unwrap();
    assert_eq!(
        child.insert("rejected").unwrap_err().sqlstate(),
        Some("53200")
    );
    assert!(!child.contains(RelationName::decoded("rejected")));
    drop(occupied);
    for cancellation in [&original, &invoking] {
        cancellation.cancel();
        assert_eq!(
            child.insert("cancelled").unwrap_err().sqlstate(),
            Some("57014")
        );
        assert!(!child.contains(RelationName::decoded("cancelled")));
        cancellation.reset();
    }
    drop(child);
    assert_eq!(budget.used(), baseline);
    drop(parent);
    assert_eq!(budget.used(), 0);
}
