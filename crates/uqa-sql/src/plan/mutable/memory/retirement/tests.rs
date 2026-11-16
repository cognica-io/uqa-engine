//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::{BudgetedVec, MemoryBudget, MemoryError};

fn chain(depth: usize, memory: &MemoryBudget) -> Result<RewriteMemory, MemoryError> {
    let mut owner = RewriteMemory::Scalar(None);
    for level in 0..depth {
        if level.is_multiple_of(2) {
            let direct = memory.reserve(8)?;
            let mut fields = BudgetedVec::new(memory);
            fields.push(RewriteField {
                name: "child",
                memory: Some(owner),
            })?;
            owner = RewriteMemory::Fields { direct, fields };
            continue;
        }
        let direct = memory.reserve(3)?;
        let mut fields = BudgetedVec::new(memory);
        fields.push(RewriteField {
            name: "absent",
            memory: None,
        })?;
        fields.push(RewriteField {
            name: "child",
            memory: Some(owner),
        })?;
        let mut items = BudgetedVec::new(memory);
        items.push(None)?;
        fields.push(RewriteField {
            name: "scalars",
            memory: Some(RewriteMemory::Scalars {
                direct: memory.reserve(5)?,
                items,
            }),
        })?;
        owner = RewriteMemory::Fields { direct, fields };
    }
    Ok(owner)
}

#[test]
fn deep_query_constructor_records_fold_and_drop_without_recursive_cleanup() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let memory = MemoryBudget::new(16 << 20);
            let sibling = memory.reserve(47).unwrap();
            let owner = chain(8192, &memory).unwrap();
            assert!(memory.used() > 47 + 8 * 8192);
            let mut folded = None;
            let measured = allocation_counter::measure(|| folded = owner.into_reservation());
            assert_eq!(measured.count_total, 0);
            assert_eq!(folded.as_ref().unwrap().bytes(), 8 * 8192);
            assert_eq!(memory.used(), 47 + 8 * 8192);
            drop(folded);
            assert_eq!(memory.used(), 47);
            let owner = chain(8192, &memory).unwrap();
            assert_eq!(allocation_counter::measure(|| drop(owner)).count_total, 0);
            assert_eq!(memory.used(), 47);
            drop(sibling);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn denied_query_constructor_records_retire_deep_partial_owners_and_keep_siblings() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for capacity in [0, 1, 256, 4096, 1 << 20] {
                let memory = MemoryBudget::new(capacity + 47);
                let sibling = memory.reserve(47).unwrap();
                assert!(chain(8192, &memory).is_err());
                assert_eq!(memory.used(), 47);
                assert!(memory.peak() <= capacity + 47);
                drop(sibling);
                assert_eq!(memory.used(), 0);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn moving_query_constructor_fields_preserves_exact_surviving_reservations() {
    let memory = MemoryBudget::new(1 << 20);
    let owner = chain(16, &memory).unwrap();
    let RewriteMemory::Fields { direct, mut fields } = owner else {
        unreachable!();
    };
    let child = fields[1].memory.take().unwrap();
    let detached = child.reserved_bytes();
    drop((fields, direct));
    assert_eq!(memory.used(), detached);
    let payload = child.into_reservation().unwrap();
    assert_eq!(payload.bytes(), 8 * 15);
    assert_eq!(memory.used(), 8 * 15);
    drop(payload);
    assert_eq!(memory.used(), 0);
}
