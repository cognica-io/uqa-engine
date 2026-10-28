//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn retained_constant_decoding_admits_before_allocating_and_releases_scratch() {
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    for resident in [false, true] {
        let storage = RetainedConstants::new(if resident { 8 * 1024 * 1024 } else { 0 });
        let value = storage
            .retain(Value::Str("x".repeat(4 * 1024 * 1024)))
            .unwrap();
        for cancelled in [None, Some(false), Some(true)] {
            let memory = MemoryBudget::new(if cancelled.is_some() {
                usize::MAX
            } else {
                1024
            });
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if let Some(original_owner) = cancelled {
                if original_owner {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
            }
            let control = ProductionControl::new(&memory, &original, &invoking);
            let mut result = None;
            let allocation = allocation_counter::measure(|| {
                result = Some(value.read_with_control(&control));
            });
            assert!(
                allocation.bytes_total < 16 * 1024,
                "resident={resident}, cancelled={cancelled:?}: {allocation:?}"
            );
            assert_eq!(
                result.unwrap().unwrap_err().sqlstate(),
                Some(if cancelled.is_some() {
                    "57014"
                } else {
                    "53200"
                })
            );
            assert_eq!(memory.used(), 0);
        }
        let memory = MemoryBudget::new(32 * 1024 * 1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let decoded = value.read_with_control(&control).unwrap();
        let Value::Str(text) = &*decoded else {
            panic!("text constant")
        };
        assert_eq!(text.len(), 4 * 1024 * 1024);
        assert_eq!(memory.used(), text.capacity());
        drop(decoded);
        assert_eq!(memory.used(), 0);
        assert!(value.read_with_control(&control).is_ok());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn admitted_constant_decoding_preserves_nested_physical_representations() {
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    let values = vec![
        Value::Float(-0.0),
        Value::Str("λ🙂".repeat(128)),
        Value::Decimal(
            uqa_core::DecimalValue::parse("123456789012345678901234567890.00100").unwrap(),
        ),
        Value::Array(
            uqa_core::ArrayValue::with_lower_bounds(vec![Value::Int(7), Value::Null], vec![-3])
                .unwrap()
                .with_element_type_oid(Some(23)),
        ),
        Value::BitString(uqa_core::BitStringValue::try_from_packed(vec![0b1010_0000], 3).unwrap()),
        Value::Enum(
            uqa_core::EnumValue::new(
                24000,
                uqa_core::EnumLabelKey::from_bytes(vec![7, 1]).unwrap(),
            )
            .with_label_oid(Some(24001)),
        ),
        Value::Datum(uqa_core::DatumValue::new(24000, 4096, vec![0; 128])),
        Value::Row(
            uqa_core::RowValue::typed(
                vec![Value::Str("row".into())],
                vec![uqa_core::RecordFieldType {
                    oid: 25,
                    type_modifier: -1,
                    collation_oid: Some(950),
                }],
            )
            .unwrap(),
        ),
        Value::Record(uqa_core::RecordValue::from_parts(
            vec![("field".into(), Value::Int(7))],
            Some(24000),
        )),
        Value::Map([(String::from("key"), Value::JsonB("{\"x\": 1}".into()))].into()),
    ];
    let expected = Value::List(values);
    for storage_bytes in [0, 65536] {
        let storage = RetainedConstants::new(storage_bytes);
        let retained = storage.retain(expected.clone()).unwrap();
        let memory = MemoryBudget::new(65536);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let decoded = retained.read_with_control(&control).unwrap();
        assert!(decoded.has_same_representation(&expected));
        assert!(memory.used() > 0);
        drop(decoded);
        assert_eq!(memory.used(), 0);
    }
}

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
