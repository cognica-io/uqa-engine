//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::index::value::ValueIndexBuilder;
use uqa_storage::memory_capture::MemoryCaptureRegistry;

fn build(allowance: usize, carried: bool) -> ValueIndexCache {
    ValueIndexBuilder::from_values(
        "stored",
        carried,
        allowance,
        [
            (0, Value::Float(-0.0)),
            (1, Value::Int(1)),
            (9, Value::Null),
            (u64::MAX, Value::Str("original".into())),
        ],
        &StorageReadControl::with_limit(1 << 20),
    )
    .unwrap()
}

#[test]
fn spilled_value_capture_keeps_wide_evaluated_values_on_native_roots() {
    let read = StorageReadControl::with_limit(4 << 20);
    let source = ValueIndexBuilder::from_values(
        "stored",
        false,
        0,
        [(7, Value::Bytes(vec![0x5a; 2 << 20]))],
        &read,
    )
    .unwrap();
    assert!(source.is_spilled());
    let small = StorageReadControl::with_limit(128 << 10);
    let mut captured = None;
    let allocated = allocation_counter::measure(|| {
        captured = Some(source.capture_values(&small).unwrap());
    });
    assert!(allocated.bytes_total < 2048, "{allocated:?}");
    let captured = captured.unwrap();
    let retained = small.memory().used();
    assert_eq!(captured.len().unwrap(), 1);
    assert!(matches!(
        captured.value(7),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(small.memory().used(), retained);
    assert_eq!(captured.field().unwrap(), "stored");
    let complete = StorageReadControl::with_limit(4 << 20);
    let saved = source.capture_values(&complete).unwrap();
    drop(source);
    let value = saved.value(7).unwrap().unwrap();
    assert!(complete.memory().used() >= 2 << 20);
    assert!(
        matches!(&*value, Value::Bytes(bytes) if bytes.len() == 2 << 20 && bytes.iter().all(|byte| *byte == 0x5a))
    );
    drop(value);
    drop((captured, saved));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(complete.memory().used(), 0);
}

#[test]
fn resident_value_capture_retains_native_roots_without_visiting_or_copying_payloads() {
    for count in [1, 4096] {
        let read = StorageReadControl::with_limit(16 << 20);
        let mut source = ValueIndexBuilder::from_values(
            "stored",
            false,
            16 << 20,
            (0..count).map(|id| (id, Value::Str(format!("{id:064}")))),
            &read,
        )
        .unwrap();
        assert!(!source.is_spilled());
        let control = StorageReadControl::with_limit(16 << 20);
        let mut captured = None;
        let allocation = allocation_counter::measure(|| {
            captured = Some(source.capture_values(&control).unwrap());
        });
        assert!(
            allocation.bytes_total < 2048,
            "{count} entries: {allocation:?}"
        );
        let captured = captured.unwrap();
        assert_eq!(captured.len().unwrap(), count as usize);
        assert!(std::ptr::eq(
            captured.value(0).unwrap().as_deref().unwrap(),
            source.policy.stored_value(0).unwrap()
        ));
        let retained = control.memory().used();
        assert!(retained >= count as usize * 64);
        let exact = StorageReadControl::with_limit(retained);
        drop(source.capture_values(&exact).unwrap());
        assert_eq!(exact.memory().used(), 0);
        let small = StorageReadControl::with_limit(retained - 1);
        assert!(matches!(
            source.capture_values(&small),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(small.memory().used(), 0);
        let mutation = allocation_counter::measure(|| {
            source
                .replace(0, Some(&Value::Str("replacement".into())), &read)
                .unwrap();
        });
        assert!(
            mutation.bytes_total < 32 * 1024,
            "{count} entries: {mutation:?}"
        );
        if count > 1 {
            assert!(std::ptr::eq(
                captured.value(1).unwrap().as_deref().unwrap(),
                source.policy.stored_value(1).unwrap()
            ));
        }
        source.set_use("carried", true);
        source.clear();
        drop(source);
        assert_eq!(
            captured.value(0).unwrap().as_deref(),
            Some(&Value::Str(format!("{:064}", 0)))
        );
        drop(captured);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn resident_value_capture_counts_complete_nested_payloads_before_root_retention() {
    let read = StorageReadControl::with_limit(16 << 20);
    let source = ValueIndexBuilder::from_values(
        "carried",
        true,
        16 << 20,
        [(
            1,
            Value::List(vec![Value::Str("x".repeat(2 << 20)), Value::Float(-0.0)]),
        )],
        &read,
    )
    .unwrap();
    assert!(!source.is_spilled());
    let small = StorageReadControl::with_limit(1 << 20);
    assert!(matches!(
        source.capture_values(&small),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(small.memory().used(), 0);
    let full = StorageReadControl::with_limit(8 << 20);
    let captured = source.capture_values(&full).unwrap();
    assert!(std::ptr::eq(
        captured.value(1).unwrap().as_deref().unwrap(),
        source.policy.stored_value(1).unwrap()
    ));
    drop(source);
    let value = captured.value(1).unwrap().unwrap();
    let Value::List(values) = &*value else {
        panic!("stored list")
    };
    assert_eq!(values[1], Value::Float(-0.0));
    full.cancellation().cancel();
    assert!(captured.value(1).is_err());
    drop(value);
    drop(captured);
    assert_eq!(full.memory().used(), 0);
}

#[test]
fn captured_value_indexes_preserve_resident_and_spilled_keys_after_source_loss() {
    for allowance in [0, 1 << 20] {
        for carried in [false, true] {
            let mut source = build(allowance, carried);
            let read = StorageReadControl::with_limit(1 << 20);
            source
                .replace(
                    2,
                    Some(&Value::Temporal(
                        uqa_core::TemporalValue::parse_date("2020-01-01").unwrap(),
                    )),
                    &read,
                )
                .unwrap();
            source.replace(2, None, &read).unwrap();
            let control = StorageReadControl::with_limit(1 << 20);
            let snapshot = source.capture_values(&control).unwrap();
            let policy = snapshot.policy().unwrap();
            assert_eq!(policy.carried, carried);
            // Resident carried values maintain no predicate tree; the spilled owner retains observed eligibility even for carried keys.
            assert_eq!(policy.has_temporal, !carried || allowance == 0);
            assert!(!policy.has_row_values);
            assert_eq!(snapshot.field().unwrap(), "stored");
            assert_eq!(snapshot.len().unwrap(), 4);
            assert!(!snapshot.is_empty().unwrap());
            assert!(snapshot.value(2).unwrap().is_none());
            assert!(matches!(
                snapshot.value(9).unwrap().as_deref(),
                Some(Value::Null)
            ));
            assert!(snapshot
                .value(0)
                .unwrap()
                .unwrap()
                .has_same_representation(&Value::Float(-0.0)));
            source.clear();
            source.set_use("replacement", !carried);
            drop(source);
            let mut ids = Vec::new();
            snapshot
                .visit(&mut |id, _| {
                    ids.push(id);
                    Ok(())
                })
                .unwrap();
            assert_eq!(ids, [0, 1, 9, u64::MAX]);
            assert_eq!(
                snapshot.value(u64::MAX).unwrap().as_deref(),
                Some(&Value::Str("original".into()))
            );
            assert!(control.memory().used() > 0);
            drop(snapshot);
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn value_index_capture_admission_keeps_membership_and_isolates_failed_cuts() {
    let registry = MemoryCaptureRegistry::new();
    let key = ValueIndexKey::Index("expression".into());
    let mut indexes = BTreeMap::new();
    indexes.insert(key.clone(), build(0, false));
    let mut derived = build(1 << 20, false);
    derived
        .replace(
            42,
            Some(&Value::Str("x".repeat(64 * 1024))),
            &StorageReadControl::with_limit(1 << 20),
        )
        .unwrap();
    indexes.insert(ValueIndexKey::Column("derived".into()), derived);
    let source = registry.cell(ValueIndexes::from(indexes)).unwrap();
    let small = StorageReadControl::with_limit(1 << 10);
    let large = StorageReadControl::with_limit(32 << 10);
    let failed = registry.capture(&small).unwrap();
    let good = registry.capture(&large).unwrap();
    let retained = source.key().unwrap();
    source.write().clear();
    assert!(failed.check().is_err());
    good.check().unwrap();
    let captured = retained.retained(&good).unwrap().unwrap();
    assert!(captured.keys().unwrap().eq(std::iter::once(&key)));
    assert_eq!(captured.get(&key).unwrap().unwrap().len().unwrap(), 4);
    drop((source, registry, good, failed));
    assert_eq!(
        captured
            .get(&key)
            .unwrap()
            .unwrap()
            .value(1)
            .unwrap()
            .as_deref(),
        Some(&Value::Int(1))
    );
    drop(captured);
    assert_eq!(large.memory().used(), 0);
    assert_eq!(small.memory().used(), 0);
}

#[test]
fn captured_value_index_limits_and_original_cancellation_are_preserved() {
    for allowance in [0, 1 << 20] {
        let source = build(allowance, false);
        let small = StorageReadControl::with_limit(16);
        assert!(matches!(
            source.capture_values(&small),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(small.memory().used(), 0);
        let control = StorageReadControl::with_limit(1 << 20);
        let captured = source.capture_values(&control).unwrap();
        let mut visits = 0;
        assert!(captured
            .visit(&mut |_, _| {
                visits += 1;
                control.cancellation().cancel();
                Ok(())
            })
            .is_err());
        assert_eq!(visits, 1);
        assert!(captured.value(1).is_err());
        assert!(captured.field().is_err());
        assert!(captured.policy().is_err());
        assert!(captured.len().is_err());
        drop(captured);
        assert_eq!(control.memory().used(), 0);
        assert!(source
            .stored_value(1, &StorageReadControl::with_limit(1 << 20))
            .unwrap()
            .is_some());
    }
}
