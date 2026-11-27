//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::Cursor;

#[test]
fn disk_key_probes_check_the_complete_key_and_keep_insertion_order_across_growth() {
    let mut index = PartitionedKeys::new(None, "join").unwrap();
    let hash = 77;
    let mut first = vec![b'x'; 8193];
    let mut second = first.clone();
    first[8192] = b'a';
    second[8192] = b'b';
    assert!(index.insert_hashed(&first, hash, Some(9)).unwrap());
    assert!(index.insert_hashed(&second, hash, Some(4)).unwrap());
    let mut old = index.positions_hashed(&first, hash).unwrap().unwrap();
    for n in 0..160_u64 {
        index.insert(&n.to_le_bytes(), Some(n)).unwrap();
    }
    assert!(!index.insert_hashed(&first, hash, Some(1)).unwrap());
    assert!(!index.insert_hashed(&second, hash, Some(6)).unwrap());
    assert!(!index.insert_hashed(&first, hash, Some(2)).unwrap());
    assert_eq!(old.next().unwrap(), Some(9));
    assert_eq!(old.next().unwrap(), None);
    for (key, expected) in [(&first, vec![9, 1, 2]), (&second, vec![4, 6])] {
        let mut rows = index.positions_hashed(key, hash).unwrap().unwrap();
        for n in expected {
            assert_eq!(rows.next().unwrap(), Some(n));
        }
        assert_eq!(rows.next().unwrap(), None);
        assert_eq!(rows.next().unwrap(), None);
    }
    assert!(index.positions_hashed(b"absent", hash).unwrap().is_none());
    assert!(index.insert(b"", None).unwrap());
    assert!(!index.insert(b"", None).unwrap());
    assert!(index.contains(b"").unwrap());
    assert!(index.positions(b"").unwrap().is_none());
}

#[test]
fn disk_key_lengths_and_position_links_reject_truncation_before_allocating() {
    for length in [0, 4] {
        let mut index = PartitionedKeys::new(None, "DISTINCT").unwrap();
        assert!(index.insert(b"key", None).unwrap());
        index.keys.set_len(length).unwrap();
        let error = index
            .insert_hashed(b"other", stable_hash(b"key"), None)
            .unwrap_err();
        assert!(error.to_string().contains("truncated spill key length"));
    }
    let mut index = PartitionedKeys::new(None, "join").unwrap();
    index.insert(b"key", Some(1)).unwrap();
    index.keys.seek(SeekFrom::Start(0)).unwrap();
    index.keys.write_all(&(1_u64 << 40).to_le_bytes()).unwrap();
    let error = index
        .positions_hashed(b"short", stable_hash(b"key"))
        .err()
        .unwrap();
    assert!(error.to_string().contains("exceeds remaining record bytes"));

    let mut index = PartitionedKeys::new(None, "join").unwrap();
    index.insert(b"key", Some(1)).unwrap();
    index.insert(b"key", Some(2)).unwrap();
    let links = index.rows.as_mut().unwrap();
    links.seek(SeekFrom::Start(8)).unwrap();
    links.write_all(&1_u64.to_le_bytes()).unwrap();
    assert!(index
        .positions(b"key")
        .unwrap()
        .unwrap()
        .next()
        .unwrap_err()
        .to_string()
        .contains("invalid spill row link"));
    index.rows.as_ref().unwrap().set_len(17).unwrap();
    assert!(index
        .positions(b"key")
        .err()
        .unwrap()
        .to_string()
        .contains("invalid spill row bounds"));
    assert!(index
        .insert(b"another", Some(3))
        .unwrap_err()
        .to_string()
        .contains("truncated spill row positions"));
    assert!(!index.contains(b"another").unwrap());
}

#[test]
fn growing_hash_partitions_bound_disk_work_for_distinct_keys_and_join_probes() {
    fn exercise(count: u64) -> (u64, u64) {
        let mut set = PartitionedKeys::new(None, "DISTINCT").unwrap();
        let mut join = PartitionedKeys::new(None, "join").unwrap();
        for n in 0..count {
            let bytes = n.to_le_bytes();
            assert!(set.insert(&bytes, None).unwrap());
            assert!(join.insert(&bytes, Some(n)).unwrap());
        }
        for n in (0..count).rev() {
            let bytes = n.to_le_bytes();
            assert!(!set.insert(&bytes, None).unwrap());
            let mut matches = join.positions(&bytes).unwrap().unwrap();
            assert_eq!(matches.next().unwrap(), Some(n));
            assert_eq!(matches.next().unwrap(), None);
        }
        assert!(set.rows.is_none());
        (set.directory.probes, join.directory.probes)
    }
    let small = exercise(256);
    let large = exercise(1024);
    for (small, large) in [(small.0, large.0), (small.1, large.1)] {
        assert!(
            large < small * 7,
            "directory reads for 256 and 1024 keys: {small}, {large}"
        );
        assert!(large < 1024 * 24, "{large} directory reads");
    }
}

#[test]
fn adjacent_positions_are_read_in_one_buffer_without_scanning_other_keys() {
    struct CountReads {
        inner: Cursor<Vec<u8>>,
        calls: usize,
    }
    impl Read for CountReads {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.calls += 1;
            self.inner.read(bytes)
        }
    }
    impl Seek for CountReads {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.inner.seek(position)
        }
    }
    let mut input = Vec::new();
    for row in 0..128_u64 {
        input.extend_from_slice(&row.to_le_bytes());
        input.extend_from_slice(&(row + 2).to_le_bytes());
    }
    let read = CountReads {
        inner: Cursor::new(input),
        calls: 0,
    };
    let mut rows = DiskPositions {
        input: BufReader::new(read),
        position: 0,
        next: 1,
        last: 128,
    };
    for n in 0..128 {
        assert_eq!(rows.next().unwrap(), Some(n));
    }
    assert_eq!(rows.next().unwrap(), None);
    assert_eq!(rows.input.get_ref().calls, 1);
}

#[test]
fn cancelled_spill_operations_keep_the_previous_partition_and_sqlstate() {
    let mut index = PartitionedKeys::new(None, "join").unwrap();
    for n in 0..48_u64 {
        index.insert(&n.to_le_bytes(), Some(n)).unwrap();
    }
    assert!(index.directory.needs_growth());
    let token = uqa_core::CancellationToken::new();
    token.cancel();
    let scope = uqa_sql::expr::StatementCancellationScope::enter(token);
    let error = index.insert(b"new key", Some(80)).unwrap_err();
    let ExecError::SQL(error) = error else {
        panic!("missing SQL cancellation")
    };
    assert_eq!(error.sqlstate(), Some("57014"));
    drop(scope);
    assert!(!index.contains(b"new key").unwrap());
    assert!(index.insert(b"new key", Some(80)).unwrap());
    for n in 0..48_u64 {
        assert_eq!(
            index
                .positions(&n.to_le_bytes())
                .unwrap()
                .unwrap()
                .next()
                .unwrap(),
            Some(n)
        );
    }
}
