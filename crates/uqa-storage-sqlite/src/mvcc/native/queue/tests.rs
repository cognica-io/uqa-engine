//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[test]
fn work_queue_seeks_keys_without_rescanning_a_constant_family() {
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE _uqa_mvcc_native_expected (family INTEGER, physical_key BLOB, PRIMARY KEY (family, physical_key))").unwrap();
    let families = [NativeRecordFamily::Documents, NativeRecordFamily::Vectors];
    for family in families {
        for number in 0_u64..1024 {
            connection
                .execute(
                    "INSERT INTO _uqa_mvcc_native_expected VALUES (?1, ?2)",
                    params![family.id(), number.to_be_bytes().as_slice()],
                )
                .unwrap();
        }
    }
    let instructions = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&instructions);
    connection
        .progress_handler(
            1,
            Some(move || {
                counter.fetch_add(1, Ordering::Relaxed);
                false
            }),
        )
        .unwrap();
    let control = StorageReadControl::with_limit(65536);
    let mut actual = Vec::new();
    let condition = format!("family = {}", families[0].id());
    visit(
        &connection,
        "_uqa_mvcc_native_expected",
        &condition,
        &control,
        |family, key| {
            actual.push((family.id(), u64::from_be_bytes(key.try_into().unwrap())));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        actual,
        (0..1024)
            .map(|number| (families[0].id(), number))
            .collect::<Vec<_>>()
    );
    assert!(
        instructions.load(Ordering::Relaxed) < 150_000,
        "queue used {} instructions",
        instructions.load(Ordering::Relaxed)
    );
    assert_eq!(control.memory().used(), 0);
    actual.clear();
    visit(
        &connection,
        "_uqa_mvcc_native_expected",
        "1",
        &control,
        |family, key| {
            actual.push((family.id(), u64::from_be_bytes(key.try_into().unwrap())));
            Ok(())
        },
    )
    .unwrap();
    let mut expected = families
        .into_iter()
        .flat_map(|family| (0..1024).map(move |number| (family.id(), number)))
        .collect::<Vec<_>>();
    expected.sort_unstable();
    assert_eq!(actual, expected);
    assert_eq!(control.memory().used(), 0);
}
