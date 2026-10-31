//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::locking::leaf_validation::LockLeafKind;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn lock_target_production_retires_partial_names_before_publishing() {
    let budget = MemoryBudget::new(16 << 20);
    let query = CancellationToken::new();
    let session = CancellationToken::new();
    let control = ProductionControl::new(&budget, &query, &session);
    let name = "x".repeat(4 << 20);
    let source = LockLeaf {
        names: [Some(&name), None, None],
        qualifier: &name,
        storage_name: control.copy_text("public.items").unwrap(),
        display_name: &name,
        kind: LockLeafKind::Base,
        nullable: false,
    };
    let baseline = budget.used();
    let occupied = budget
        .reserve(budget.available() - (4 << 20) - (64 << 10))
        .unwrap();
    let mut rejected = None;
    let allocation = allocation_counter::measure(|| {
        rejected = Some(retain_target(
            &source,
            LockStrength::ForUpdate,
            LockWait::Block,
            &control,
        ));
    });
    assert!(
        allocation.bytes_total >= 4 << 20,
        "the qualifier must be admitted before display-name failure"
    );
    assert!(
        allocation.bytes_total < (4 << 20) + (128 << 10),
        "{allocation:?}"
    );
    assert_eq!(rejected.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert_eq!(budget.used(), baseline + occupied.bytes());
    drop(occupied);
    let target =
        retain_target(&source, LockStrength::ForUpdate, LockWait::Block, &control).unwrap();
    assert_eq!(budget.used(), baseline + target.reserved_bytes());
    assert_eq!(target.qualifier, name);
    assert_eq!(target.display_name, name);
    assert_eq!(target.storage_name, "public.items");
    assert!(!target.identity_source);
    drop(target);
    for token in [&query, &session] {
        token.cancel();
        assert_eq!(
            retain_target(&source, LockStrength::ForUpdate, LockWait::Block, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), baseline);
        token.reset();
    }
    drop(source);
    assert_eq!(budget.used(), 0);
}
