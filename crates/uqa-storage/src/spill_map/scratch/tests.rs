//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn scheduled_scratch_keeps_fixed_update_work_beyond_compaction_thresholds() {
    let control = StorageReadControl::with_limit(32 << 10).with_temporary_limit(32 << 20);
    let mut map = Scratch::new(&control).unwrap();
    for step in 0..8192_u64 {
        let key = step % 257;
        let before = map.disk.as_ref().map_or(0, disk::Map::written_bytes);
        map.insert(u128::from(key), step).unwrap();
        // The 257-key trie has at most nine ancestors. An update fits two authenticated 1-KiB blocks even after its file exceeds the ordinary map's compaction threshold.
        let after = map.disk.as_ref().map_or(0, disk::Map::written_bytes);
        assert!(after - before <= 2 * (1024 + 24 + 2 + 16 + 1));
        assert_eq!(map.get(u128::from(key)).unwrap(), Some(step));
    }
    for key in 0..257_u64 {
        let last = key + (8191 - key) / 257 * 257;
        assert_eq!(map.get(u128::from(key)).unwrap(), Some(last));
    }
    assert!(control.file_budget().unwrap().usage().0 > 1 << 20);
    let path = map.disk.as_ref().unwrap().path();
    drop(map);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(control.file_budget().unwrap().usage().0, 0);
    assert!(!path.exists());
}

#[test]
fn scheduled_scratch_shares_file_quota_and_retires_denied_or_cancelled_work() {
    let control = StorageReadControl::with_limit(64 << 10).with_temporary_limit(12 << 10);
    let mut first = Scratch::new(&control).unwrap();
    let mut second = Scratch::new(&control.clone()).unwrap();
    first.insert(1, 17).unwrap();
    second.insert(2, 19).unwrap();
    for key in 1000..1060 {
        second.insert(key, 31).unwrap();
    }
    let used = control.file_budget().unwrap().usage().0;
    assert!(used > 0 && used <= 12 << 10);
    let mut failed = false;
    for key in 3..128 {
        if first.insert(key, 23).is_err() {
            failed = true;
            assert_eq!(first.get(key).unwrap(), None);
            break;
        }
    }
    assert!(failed);
    assert_eq!(first.get(1).unwrap(), Some(17));
    assert_eq!(second.get(2).unwrap(), Some(19));
    control.cancellation().cancel();
    assert!(first.get(1).is_err());
    assert!(second.insert(3, 29).is_err());
    drop((first, second));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(control.file_budget().unwrap().usage().0, 0);
    assert!(control.file_budget().unwrap().usage().1 <= 12 << 10);
    let denied = StorageReadControl::with_limit(64 << 10).with_temporary_limit(0);
    let mut map = Scratch::new(&denied).unwrap();
    assert!((0..128).any(|key| map.insert(key, 37).is_err()));
    drop(map);
    assert_eq!(denied.memory().used(), 0);
    assert_eq!(denied.file_budget().unwrap().usage().0, 0);
}
