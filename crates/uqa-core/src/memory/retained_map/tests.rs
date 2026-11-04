//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn retained_map_captures_nonclone_payloads_without_allocating_or_visiting_entries() {
    struct Payload(usize);
    for entries in [1, 64, 4096] {
        let construction = MemoryBudget::new(8 << 20);
        let mut source = RetainedMap::new(&construction);
        for key in 0..entries {
            source.insert(key, Payload(key * 2), 1024).unwrap();
        }
        let first = MemoryBudget::new(8 << 20);
        let second = MemoryBudget::new(8 << 20);
        let mut saved = None;
        let usage = allocation_counter::measure(|| saved = Some(source.capture(&first).unwrap()));
        assert_eq!(usage.count_total, 0);
        let saved = saved.unwrap();
        let independent = source.capture(&second).unwrap();
        assert_eq!(first.used(), second.used());
        assert!(first.used() >= entries * 1024);
        let original = source.clone();
        source.insert(0, Payload(99), 8192).unwrap();
        source.clear();
        drop((source, original));
        assert_eq!(saved.get(&0).unwrap().0, 0);
        assert!(saved
            .iter()
            .map(|(key, value)| (*key, value.0))
            .eq((0..entries).map(|key| (key, key * 2))));
        drop(saved);
        assert_eq!(first.used(), 0);
        assert!(second.used() > 0);
        drop(independent);
        assert_eq!(second.used(), 0);
        assert_eq!(construction.used(), 0);
    }
}

#[test]
fn retained_map_failed_updates_and_overflow_preserve_root_and_footprint() {
    let construction = MemoryBudget::new(8192);
    let capture = MemoryBudget::new(8192);
    let mut source = RetainedMap::new(&construction);
    source.insert("a".to_owned(), 1, 100).unwrap();
    source.insert("b".to_owned(), 2, 200).unwrap();
    let saved = source.capture(&capture).unwrap();
    let retained = capture.used();
    let occupied = construction.reserve(construction.available()).unwrap();
    assert!(source.insert("a".into(), 3, 300).is_err());
    assert!(source.insert("c".into(), 3, 300).is_err());
    assert!(source.remove("b").is_err());
    assert!(!source.remove("missing").unwrap());
    assert!(matches!(
        source.insert("c".into(), 4, usize::MAX),
        Err(MemoryError::SizeOverflow)
    ));
    drop(occupied);
    let after = source.capture(&capture).unwrap();
    assert_eq!(capture.used(), retained * 2);
    assert!(saved.iter().eq(after.iter()));
    source.insert("a".into(), 9, 500).unwrap();
    let replaced = source.capture(&capture).unwrap();
    assert_eq!(replaced.reserved_bytes(), retained + 400);
    assert!(source.remove("b").unwrap());
    assert_eq!(source.get("a"), Some(&9));
    assert!(source.get("b").is_none());
    drop((source, saved, after, replaced));
    assert_eq!(capture.used(), 0);
    assert_eq!(construction.used(), 0);
}

#[test]
fn retained_map_capture_admits_exact_root_and_releases_failed_admission() {
    let mut source = RetainedMap::new(&MemoryBudget::new(4096));
    source.insert("one", (), 512).unwrap();
    let first = source.capture(&MemoryBudget::new(4096)).unwrap();
    let bytes = first.reserved_bytes();
    let denied = MemoryBudget::new(bytes - 1);
    assert!(source.capture(&denied).is_err());
    assert_eq!(denied.used(), 0);
    let exact = MemoryBudget::new(bytes);
    let second = source.capture(&exact).unwrap();
    assert_eq!(exact.used(), bytes);
    source.insert("one", (), usize::MAX).unwrap();
    assert!(matches!(
        source.capture(&MemoryBudget::new(usize::MAX)),
        Err(MemoryError::SizeOverflow)
    ));
    assert_eq!(first.len(), 1);
    source.clear();
    assert!(source.capture(&MemoryBudget::new(0)).unwrap().is_empty());
    drop(second);
    assert_eq!(exact.used(), 0);
}
