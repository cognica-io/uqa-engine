//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use super::*;
use crate::{spill::SpillBuffer, RowSchema};
use uqa_core::Value;

fn batch(value: u8, bytes: usize) -> Batch {
    Batch::new(
        RowSchema::new(vec!["value".into()]),
        vec![BTreeMap::from([(
            "value".into(),
            Value::Bytes(vec![value; bytes]),
        )])],
    )
}

fn values(buffer: &SpillBuffer) -> Vec<Value> {
    buffer
        .reader()
        .unwrap()
        .flat_map(|batch| batch.unwrap().into_result_rows())
        .map(|mut row| row.remove("value").unwrap())
        .collect()
}

#[test]
fn shared_segments_bound_readers_and_reject_appends_after_another_segment() {
    let directory = tempfile::tempdir().unwrap();
    let arena = SpillFileArena::new(Some(directory.path().to_path_buf()));
    let mut first = SpillBuffer::new(1).with_arena(&arena);
    first.push(batch(1, 17)).unwrap();
    first.push(batch(2, 32 << 10)).unwrap();
    let mut second = SpillBuffer::new(1).with_arena(&arena);
    second.push(batch(3, 23)).unwrap();
    assert_eq!(first.spill_path(), second.spill_path());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    assert_eq!(
        values(&first),
        [Value::Bytes(vec![1; 17]), Value::Bytes(vec![2; 32 << 10])]
    );
    assert_eq!(values(&second), [Value::Bytes(vec![3; 23])]);
    let length = first.spill_file.as_ref().unwrap().metadata().unwrap().len();
    assert!(first.push(batch(4, 29)).is_err());
    assert_eq!(
        first.spill_file.as_ref().unwrap().metadata().unwrap().len(),
        length
    );
    assert_eq!(first.in_memory_rows(), 1);
    assert_eq!(values(&second), [Value::Bytes(vec![3; 23])]);
    let shared = second
        .into_shared(RowSchema::new(vec!["value".into()]))
        .unwrap();
    drop((arena, first));
    for _ in 0..2 {
        let batches = shared
            .reader()
            .unwrap()
            .collect::<ExecResult<Vec<_>>>()
            .unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(
            batches[0].clone().into_result_rows()[0]["value"],
            Value::Bytes(vec![3; 23])
        );
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    drop(shared);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}
