//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::MemoryBudget;

#[test]
fn indexed_field_lending_keeps_wide_sibling_measurements() {
    let native = MemoryBudget::new(2 << 20);
    let mut children = BudgetedVec::new(&native);
    for _ in 0..4096 {
        children
            .push(RewriteField {
                name: "item",
                memory: Some(RewriteMemory::direct(native.reserve(1).unwrap())),
            })
            .unwrap();
    }
    let mut owner = RewriteMemory::fields(native.reserve(7).unwrap(), children);
    let RewriteMemory::Fields { fields, .. } = &mut owner else {
        unreachable!()
    };
    assert_eq!(size_of_val(fields), size_of::<BudgetedVec<RewriteField>>());
    assert_eq!(owner.reserved_bytes(), native.used());
    let exhausted = native.reserve(native.available()).unwrap();
    let measured = allocation_counter::measure(|| {
        for end in (0..4096).rev().step_by(2) {
            for index in [end, end - 1] {
                drop(owner.item_mut(index).take());
                let RewriteMemory::Fields { fields, .. } = &mut owner else {
                    unreachable!()
                };
                assert_eq!(fields.pending, PendingFields::single(index));
            }
            assert_eq!(owner.reserved_bytes(), native.used() - exhausted.bytes());
        }
    });
    assert_eq!(measured.count_total, 0);
    drop(owner);
    assert_eq!(native.used(), exhausted.bytes());
}

#[test]
fn interrupted_footprint_observation_restores_return_slots_before_reuse() {
    let native = MemoryBudget::new(1 << 20);
    let mut owner = RewriteMemory::direct(native.reserve(11).unwrap());
    for _ in 0..64 {
        let mut fields = BudgetedVec::new(&native);
        fields
            .push(RewriteField {
                name: "child",
                memory: Some(owner),
            })
            .unwrap();
        owner = RewriteMemory::fields(native.reserve(3).unwrap(), fields);
    }
    let expected = native.used();
    let owner = RootMemory::from(Some(owner));
    assert!(std::panic::catch_unwind(|| {
        let mut root = owner.0.write().unwrap();
        let mut observation = Observation::new(&mut root);
        for _ in 0..32 {
            observation.path.descend(&mut observation.current, 0);
        }
        panic!("interrupted observation");
    })
    .is_err());
    let observed = allocation_counter::measure(|| assert_eq!(owner.reserved_bytes(), expected));
    assert_eq!(observed.count_total, 0);
    drop(owner);
    assert_eq!(native.used(), 0);
}
