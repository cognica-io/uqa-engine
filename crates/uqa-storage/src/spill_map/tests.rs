//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeMap;

fn entries(map: &Map<u64>) -> Vec<(u128, u64)> {
    let mut output = Vec::new();
    let mut after = None;
    while let Some((key, value)) = map.next(after).unwrap() {
        output.push((key, *value));
        after = Some(key);
    }
    output
}

#[test]
fn resident_and_spilled_roots_preserve_ordered_lookup_and_independent_mutations() {
    let memory = MemoryBudget::new(256 * 1024);
    let mut resident = Map::new(&memory, 128 * 1024);
    let mut disk = Map::new(&memory, 128);
    let mut expected = BTreeMap::new();
    let keys = [
        0,
        u128::MAX,
        1 << 127,
        1,
        u128::from(u64::MAX),
        (1 << 96) + 8,
        99,
        8,
        2,
        u128::MAX - 1,
    ];
    for (position, key) in keys.into_iter().enumerate() {
        for map in [&mut resident, &mut disk] {
            map.insert(key, position as u64, None).unwrap();
        }
        expected.insert(key, position as u64);
    }
    assert!(!resident.is_spilled());
    assert!(disk.is_spilled());
    let old_resident = resident.clone();
    let old_disk = disk.clone();
    let before = expected.iter().map(|(&k, &v)| (k, v)).collect::<Vec<_>>();
    for map in [&mut resident, &mut disk] {
        map.remove(0, None).unwrap();
        map.remove(u128::MAX, None).unwrap();
        map.remove(4, None).unwrap();
        map.insert(99, 100, None).unwrap();
        map.insert(4, 4, None).unwrap();
    }
    expected.remove(&0);
    expected.remove(&u128::MAX);
    expected.insert(99, 100);
    expected.insert(4, 4);
    let after = expected.into_iter().collect::<Vec<_>>();
    for map in [&resident, &disk] {
        assert_eq!(
            map.last().unwrap().map(|(key, value)| (key, *value)),
            after.last().copied()
        );
    }
    assert_eq!(entries(&resident), after);
    assert_eq!(entries(&disk), after);
    assert_eq!(entries(&old_resident), before);
    assert_eq!(entries(&old_disk), before);
    for &(key, value) in &after {
        assert_eq!(*resident.get(key).unwrap().unwrap(), value);
        assert_eq!(*disk.get(key).unwrap().unwrap(), value);
        for map in [&resident, &disk] {
            assert_eq!(
                map.next_key(Some(key), &memory).unwrap(),
                after
                    .iter()
                    .map(|entry| entry.0)
                    .find(|candidate| *candidate > key)
            );
        }
        assert_eq!(
            disk.next(Some(key))
                .unwrap()
                .map(|(key, value)| (key, *value)),
            after
                .iter()
                .copied()
                .find(|(candidate, _)| *candidate > key)
        );
    }
    drop((resident, disk, old_resident, old_disk));
    assert_eq!(memory.used(), 0);
}

#[test]
fn large_key_sets_spill_without_a_resident_directory_and_reclaim_the_file() {
    let memory = MemoryBudget::new(32 * 1024);
    let mut map = Map::new(&memory, 1024);
    let mut expected = BTreeMap::new();
    for i in 0..400_u64 {
        let key = u128::from(i.wrapping_mul(0x9e37_79b9_7f4a_7c15)) << 47 | u128::from(i);
        map.insert(key, i, None).unwrap();
        expected.insert(key, i);
    }
    let Root::Disk(disk) = &map.root else {
        panic!("expected spill");
    };
    let path = disk.path();
    assert!(path.exists());
    assert_eq!(entries(&map), expected.into_iter().collect::<Vec<_>>());
    assert!(memory.peak() <= memory.limit());
    drop(map);
    assert_eq!(memory.used(), 0);
    assert!(!path.exists());
}

#[test]
fn cancellation_and_failed_disk_append_keep_the_original_root() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut map = Map::new(control.memory(), 128);
    map.insert(1, 10_u64, Some(&control)).unwrap();
    map.insert(2, 20, Some(&control)).unwrap();
    let original = entries(&map);
    control.cancellation().cancel();
    assert!(map.insert(3, 30, Some(&control)).is_err());
    control.cancellation().reset();
    let Root::Disk(disk) = &map.root else {
        panic!("expected spill");
    };
    disk.fail_write_after(13);
    assert!(map.insert(3, 30, Some(&control)).is_err());
    assert_eq!(entries(&map), original);
    drop(map);
    assert_eq!(control.memory().used(), 0);
}
