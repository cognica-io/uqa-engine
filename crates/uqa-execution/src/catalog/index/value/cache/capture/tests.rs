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
            assert!(matches!(snapshot.value(9).unwrap(), Some(Value::Null)));
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
                snapshot.value(u64::MAX).unwrap(),
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
        captured.get(&key).unwrap().unwrap().value(1).unwrap(),
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
