//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata streams keep historical revisions and stop before producing a key after failure.

use super::*;
use uqa_storage::mvcc::{RecordMetadata, RecordMetadataRequests};

struct Requests<'a> {
    keys: &'a [&'a [u8]],
    produced: usize,
    accepted: Vec<Option<RecordMetadata>>,
    fail_after: Option<usize>,
    cancel_after: Option<usize>,
    control: &'a StorageReadControl,
}

impl<'a> Requests<'a> {
    fn new(keys: &'a [&'a [u8]], control: &'a StorageReadControl) -> Self {
        Self {
            keys,
            produced: 0,
            accepted: Vec::new(),
            fail_after: None,
            cancel_after: None,
            control,
        }
    }
}

impl RecordMetadataRequests for Requests<'_> {
    fn advance(&mut self) -> VersionResult<bool> {
        if self.produced == self.keys.len() {
            return Ok(false);
        }
        self.produced += 1;
        Ok(true)
    }

    fn key(&self) -> &[u8] {
        self.keys[self.produced - 1]
    }

    fn accept(&mut self, record: Option<RecordMetadata>) -> VersionResult<()> {
        if self.fail_after == Some(self.accepted.len()) {
            return Err(VersionError::InvalidEncoding("metadata consumer failed"));
        }
        self.accepted.push(record);
        if self.cancel_after == Some(self.accepted.len()) {
            self.control.cancellation().cancel();
        }
        Ok(())
    }
}

#[test]
fn metadata_requests_keep_history_without_payloads_and_stop_on_error_or_cancellation() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let first = store.allocate_transaction(&control).unwrap();
    let payload = vec![0x73; 128 << 10];
    store
        .commit(first, &prepared(b"key", &payload, &control), &control)
        .unwrap();
    let live = store.snapshot(&control).unwrap();
    let deletion = PreparedRecordCommit::new(
        &[RecordWrite {
            key: b"key",
            expected: Some(live.sequence()),
            value: None,
        }],
        &control,
    )
    .unwrap();
    let second = store.allocate_transaction(&control).unwrap();
    store.commit(second, &deletion, &control).unwrap();
    let deleted = store.snapshot(&control).unwrap();
    let limited = StorageReadControl::with_limit(8192);
    let keys: &[&[u8]] = &[b"key", b"missing", b"key"];
    for (snapshot, is_live) in [(&live, true), (&deleted, false)] {
        let before = RECORD_READS.with(std::cell::Cell::get);
        let mut requests = Requests::new(keys, &limited);
        snapshot.visit_metadata(&mut requests, &limited).unwrap();
        assert_eq!(RECORD_READS.with(std::cell::Cell::get) - before, 1);
        let expected = Some(RecordMetadata {
            revision: Some(snapshot.sequence()),
            live: is_live,
        });
        assert_eq!(requests.accepted, [expected, None, expected]);
        assert_eq!(requests.produced, 3);
        assert_eq!(limited.memory().used(), 0);
    }
    let before = RECORD_READS.with(std::cell::Cell::get);
    deleted
        .visit_metadata(&mut Requests::new(&[], &limited), &limited)
        .unwrap();
    assert_eq!(RECORD_READS.with(std::cell::Cell::get), before);
    for cancel in [false, true] {
        let mut requests = Requests::new(keys, &limited);
        if cancel {
            requests.cancel_after = Some(1);
        } else {
            requests.fail_after = Some(0);
        }
        let error = live.visit_metadata(&mut requests, &limited).unwrap_err();
        if !cancel {
            assert!(matches!(
                error,
                VersionError::InvalidEncoding("metadata consumer failed")
            ));
        }
        assert_eq!(requests.produced, 1);
        assert_eq!(requests.accepted.len(), usize::from(cancel));
        limited.cancellation().reset();
        assert_eq!(limited.memory().used(), 0);
    }
    assert!(limited.memory().peak() <= limited.memory().limit());
}
