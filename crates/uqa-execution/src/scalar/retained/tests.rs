//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn planning_memory_counts_resident_constants_once_and_excludes_spilled_payload() {
    use uqa_core::memory::AllocationUsage;
    let storage = RetainedConstants::new(4096);
    let retained = storage.retain(Value::Str("x".repeat(512))).unwrap();
    let control = ProductionControl::uncontrolled();
    let resident = AllocationUsage::measure(&retained, &control).unwrap();
    assert_eq!(
        resident.allocated_bytes,
        size_of::<ConstantRows>() + storage.source.0.lock().memory().used()
    );
    assert!(resident.used_bytes > 512);
    let copies = vec![retained.clone(); 128];
    let shared = AllocationUsage::measure(&copies, &control).unwrap();
    assert_eq!(
        shared.used_bytes,
        resident.used_bytes + copies.len() * size_of::<RetainedConstantValue>()
    );
    assert_eq!(
        shared.allocated_bytes,
        resident.allocated_bytes + copies.capacity() * size_of::<RetainedConstantValue>()
    );
    let large = storage.retain(Value::Str("y".repeat(1 << 20))).unwrap();
    assert!(storage.source.0.lock().spilled_bytes() > 1 << 20);
    let allocations = allocation_counter::measure(|| {
        let spilled = AllocationUsage::measure(&large, &control).unwrap();
        assert_eq!(spilled.used_bytes, size_of::<ConstantRows>());
        assert_eq!(spilled.allocated_bytes, size_of::<ConstantRows>());
    });
    assert!(allocations.bytes_max < 4096, "{allocations:?}");
    assert_eq!(allocations.bytes_current, 0);
    assert_eq!(retained.read().unwrap(), Value::Str("x".repeat(512)));
    assert_eq!(large.read().unwrap(), Value::Str("y".repeat(1 << 20)));
}

#[test]
fn constants_share_bounded_spill_across_plan_and_worker_lifetimes() {
    let values = RetainedConstants::new(4096);
    let measure = allocation_counter::measure(|| {
        let mut handles = Vec::new();
        for n in 0..128 {
            handles.push(
                values
                    .retain(Value::Str(format!("{n}:{}", "x".repeat(131_072))))
                    .unwrap(),
            );
        }
        assert!(values.source.0.lock().spilled_bytes() > 16 * 1024 * 1024);
        let expected = Value::Str(format!("127:{}", "x".repeat(131_072)));
        let shared = handles.last().unwrap().clone();
        let weak = Arc::downgrade(&values.source);
        drop(values);
        drop(handles);
        assert!(weak.upgrade().is_some());
        assert_eq!(
            std::thread::spawn(move || shared.read().unwrap())
                .join()
                .unwrap(),
            expected
        );
        assert!(weak.upgrade().is_none());
    });
    assert!(
        measure.bytes_max < 2 * 1024 * 1024,
        "{} retained bytes",
        measure.bytes_max
    );
}
