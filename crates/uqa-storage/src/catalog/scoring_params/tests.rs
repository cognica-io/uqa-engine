//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn scoring_capture_admission_does_not_visit_or_copy_catalog_payloads() {
    for entries in [1, 64, 4096] {
        let mut source = ScoringParameters::default();
        for index in 0..entries {
            source
                .insert(format!("signal-{index:05}"), "x".repeat(1024))
                .unwrap();
        }
        let first = StorageReadControl::with_limit(16 << 20);
        let second = StorageReadControl::with_limit(16 << 20);
        let mut saved = None;
        let measured = allocation_counter::measure(|| {
            saved = Some(source.capture(&first).unwrap());
        });
        assert_eq!(measured.count_total, 0, "{entries}");
        assert_eq!(measured.bytes_total, 0, "{entries}");
        let saved = saved.unwrap();
        let independently_funded = source.capture(&second).unwrap();
        assert_eq!(first.memory().used(), second.memory().used());
        assert!(first.memory().used() >= entries * (1024 + "signal-00000".len()));
        assert!(saved.budget().shares_allowance(first.memory()));
        assert!(independently_funded
            .budget()
            .shares_allowance(second.memory()));
        drop(source);
        assert_eq!(saved.len(), entries);
        assert!(saved.iter().eq(independently_funded.iter()));
        drop(saved);
        assert_eq!(first.memory().used(), 0);
        assert!(second.memory().used() > 0);
        drop(independently_funded);
        assert_eq!(second.memory().used(), 0);
    }
}

#[test]
fn scoring_root_updates_share_unrelated_payloads_and_preserve_native_bytes() {
    let mut source = ScoringParameters::default();
    for index in 0..4096 {
        source
            .insert(format!("signal-{index:05}"), "x".repeat(1024))
            .unwrap();
    }
    let key = "signal-02048";
    source
        .insert(key.into(), "{ \"x\": 1.00, \"x\": 2 }".into())
        .unwrap();
    let original = source.clone();
    let control = StorageReadControl::with_limit(16 << 20);
    let saved = source.capture(&control).unwrap();
    let retained = control.memory().used();
    let untouched = source.get("signal-00000").unwrap().as_ptr();
    let replacement = "{\"changed\":true}".to_owned();
    let key_owned = key.to_owned();
    let measured = allocation_counter::measure(|| source.insert(key_owned, replacement).unwrap());
    assert!(measured.bytes_total < 32768, "{measured:?}");
    assert_eq!(source.get("signal-00000").unwrap().as_ptr(), untouched);
    assert_eq!(original.get("signal-00000").unwrap().as_ptr(), untouched);
    assert_eq!(saved.get(key).unwrap(), "{ \"x\": 1.00, \"x\": 2 }");
    let measured = allocation_counter::measure(|| assert!(source.remove("signal-00001").unwrap()));
    assert!(measured.bytes_total < 32768, "{measured:?}");
    assert!(source.get("signal-00001").is_none());
    assert!(saved.get("signal-00001").is_some());
    source.insert("later".into(), "y".repeat(4 << 20)).unwrap();
    assert_eq!(control.memory().used(), retained);
    assert!(saved.get("later").is_none());
    source = original;
    assert!(source.iter().eq(saved.iter()));
    source.clear();
    assert!(source.is_empty());
    assert_eq!(saved.len(), 4096);
    drop((source, saved));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn scoring_failed_mutations_preserve_membership_and_retention_accounting() {
    let memory = MemoryBudget::new(8192);
    let mut source = ScoringParameters::new(&memory);
    source
        .replace([
            ("a".into(), "first".into()),
            ("b".into(), "second".into()),
            ("c".into(), "third".into()),
        ])
        .unwrap();
    let original = source.clone();
    let control = StorageReadControl::with_limit(16384);
    let saved = source.capture(&control).unwrap();
    let retained = control.memory().used();
    let occupied = memory.reserve(memory.available()).unwrap();
    assert!(source.insert("a".into(), "replacement".into()).is_err());
    assert!(source.insert("d".into(), "additional".into()).is_err());
    assert!(source.remove("b").is_err());
    assert!(source.replace([("new".into(), "new".into())]).is_err());
    assert!(!source.remove("absent").unwrap());
    assert!(source.iter().eq(original.iter()));
    drop(occupied);
    let after = source.capture(&control).unwrap();
    assert_eq!(control.memory().used(), retained * 2);
    assert!(source.remove("b").unwrap());
    source
        .replace([
            ("z".into(), "last".into()),
            ("z".into(), "replacement".into()),
        ])
        .unwrap();
    assert_eq!(source.len(), 1);
    assert_eq!(source.get("z").unwrap(), "replacement");
    assert!(after.iter().eq(saved.iter()));
    drop((source, original, saved, after));
    assert_eq!(memory.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn scoring_capture_charges_string_capacities_and_rejects_denial_or_cancellation() {
    let mut source = ScoringParameters::default();
    let empty = StorageReadControl::with_limit(0);
    assert!(source.capture(&empty).unwrap().is_empty());
    let mut name = String::with_capacity(8192);
    name.push('a');
    let mut encoded = String::with_capacity(32768);
    encoded.push_str("{}");
    let capacity = name.capacity() + encoded.capacity();
    source.insert(name, encoded).unwrap();
    let control = StorageReadControl::with_limit(65536);
    let saved = source.capture(&control).unwrap();
    let required = control.memory().used();
    assert!(required > capacity);
    let denied = StorageReadControl::with_limit(required - 1);
    assert!(source.capture(&denied).is_err());
    assert_eq!(denied.memory().used(), 0);
    let exact = StorageReadControl::with_limit(required);
    let second = source.capture(&exact).unwrap();
    assert!(saved.iter().eq(second.iter()));
    control.cancellation().cancel();
    assert!(source.capture(&control).is_err());
    assert_eq!(control.memory().used(), required);
    source.insert("a".into(), "short".into()).unwrap();
    let smaller = StorageReadControl::with_limit(4096);
    let updated = source.capture(&smaller).unwrap();
    assert!(smaller.memory().used() < required);
    assert_eq!(saved.get("a").unwrap(), "{}");
    assert_eq!(updated.get("a").unwrap(), "short");
    drop((source, saved, second, updated));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(exact.memory().used(), 0);
    assert_eq!(smaller.memory().used(), 0);
}
