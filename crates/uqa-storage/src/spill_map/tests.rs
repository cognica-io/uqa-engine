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
fn ordered_construction_spills_linearly_and_preserves_unordered_replacements() {
    for count in [128_u128, 512] {
        let control = StorageReadControl::with_limit(64 * 1024);
        let mut builder = Builder::new(control.memory(), 1024);
        for id in 0..count {
            builder.insert(id, id as u64, Some(&control)).unwrap();
        }
        let map = builder.finish(Some(&control)).unwrap();
        let Root::Disk(disk) = &map.root else {
            panic!("expected spill")
        };
        let logical_bytes = count as u64 * 136;
        assert_eq!(disk.read_blocks(), 0);
        assert_eq!(
            disk.written_bytes(),
            logical_bytes + logical_bytes.div_ceil(1024) * 43
        );
        assert_eq!(
            entries(&map),
            (0..count).map(|id| (id, id as u64)).collect::<Vec<_>>()
        );
        drop(map);
        for resident_bytes in [1024, 32 * 1024] {
            let mut builder = Builder::new(control.memory(), resident_bytes);
            let mut expected = BTreeMap::new();
            for id in (0..count).chain([17, 3, count + 1, 0]) {
                let value = expected.len() as u64;
                builder.insert(id, value, Some(&control)).unwrap();
                expected.insert(id, value);
            }
            let map = builder.finish(Some(&control)).unwrap();
            assert_eq!(entries(&map), expected.into_iter().collect::<Vec<_>>());
        }
        let mut builder = Builder::new(control.memory(), 1024);
        builder.insert(1, 1_u64, Some(&control)).unwrap();
        control.cancellation().cancel();
        assert!(builder.insert(2, 2, Some(&control)).is_err());
        control.cancellation().reset();
        assert!(builder.finish(Some(&control)).is_err());
        assert_eq!(control.memory().used(), 0);
        assert!(control.memory().peak() <= control.memory().limit());
    }
}

#[test]
fn bulk_spill_publishes_each_authenticated_block_once() {
    for count in [127_u128, 512] {
        let memory = MemoryBudget::new(32 * 1024);
        let mut builder = disk::Builder::new(&memory).unwrap();
        for key in 0..count {
            builder.push(key, &(key as u64), &memory).unwrap();
        }
        let disk = builder.finish().unwrap();
        let logical_bytes = count as u64 * (64 + 8) + (count as u64 - 1) * 64 + 64;
        let blocks = logical_bytes.div_ceil(1024);
        assert_eq!(disk.read_blocks(), 0);
        assert_eq!(
            disk.written_bytes(),
            logical_bytes + blocks * (24 + 2 + 16 + 1)
        );
        for key in 0..count {
            assert_eq!(*disk.get::<u64>(key, &memory).unwrap().unwrap(), key as u64);
        }
        assert!(memory.peak() <= memory.limit());
        drop(disk);
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(32 * 1024);
    let mut builder = disk::Builder::new(&memory).unwrap();
    builder.push(1, &17_u64, &memory).unwrap();
    let path = builder.path();
    builder.fail_write_after(13);
    assert!(builder.finish().is_err());
    assert!(
        !path.exists(),
        "a failed final flush must not retain the unpublished map"
    );
    assert_eq!(memory.used(), 0);
}

#[test]
fn repeated_spilled_reads_reuse_authenticated_blocks_across_immutable_roots() {
    let control = StorageReadControl::with_limit(512 * 1024);
    let mut map = Map::new(control.memory(), 128);
    for id in 0..512 {
        map.insert(id, id as u64, Some(&control)).unwrap();
    }
    let retained = map.clone();
    assert_eq!(*map.get(73).unwrap().unwrap(), 73);
    let Root::Disk(disk) = &map.root else {
        panic!("expected spill")
    };
    let before = disk.read_blocks();
    for _ in 0..32 {
        assert_eq!(*map.get(73).unwrap().unwrap(), 73);
    }
    assert_eq!(
        disk.read_blocks(),
        before,
        "warm reads must not decrypt the same blocks again"
    );
    map.insert(73, 900, Some(&control)).unwrap();
    assert_eq!(*map.get(73).unwrap().unwrap(), 900);
    assert_eq!(*retained.get(73).unwrap().unwrap(), 73);
    let Root::Disk(disk) = &map.root else {
        unreachable!()
    };
    disk.fail_write_after(100);
    assert!(map.insert(73, 901, Some(&control)).is_err());
    assert_eq!(*map.get(73).unwrap().unwrap(), 900);
    map.insert(73, 902, Some(&control)).unwrap();
    assert_eq!(*map.get(73).unwrap().unwrap(), 902);
    assert_eq!(*retained.get(73).unwrap().unwrap(), 73);
    assert!(control.memory().peak() <= control.memory().limit());
    drop((map, retained));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn disk_replacements_publish_adjacent_path_headers_once_per_block() {
    for count in [128, 512] {
        let control = StorageReadControl::with_limit(32 * 1024);
        let mut map = Map::new(control.memory(), 128);
        for id in 0..count {
            map.insert(id, id as u64, Some(&control)).unwrap();
        }
        let retained = map.clone();
        let Root::Disk(disk) = &map.root else {
            panic!("expected spill")
        };
        let before = disk.written_bytes();
        map.insert(73, 900, Some(&control)).unwrap();
        let Root::Disk(disk) = &map.root else {
            unreachable!()
        };
        let written = disk.written_bytes() - before;
        // One 72-byte leaf and at most nine 64-byte ancestors span at most two
        // 1024-byte blocks, each with nonce/length/tag and one publication byte.
        assert!(written <= 2 * (1024 + 24 + 2 + 16 + 1), "{written}");
        assert_eq!(
            entries(&retained),
            (0..count).map(|id| (id, id as u64)).collect::<Vec<_>>()
        );
        let expected = (0..count)
            .map(|id| (id, if id == 73 { 900 } else { id as u64 }))
            .collect::<Vec<_>>();
        assert_eq!(entries(&map), expected);
        for failed_after in [0, 42, 100] {
            let Root::Disk(disk) = &map.root else {
                unreachable!()
            };
            disk.fail_write_after(failed_after);
            assert!(map.insert(73, 901, Some(&control)).is_err());
            assert_eq!(entries(&map), expected);
        }
        drop((map, retained));
        assert_eq!(control.memory().used(), 0);
    }
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
