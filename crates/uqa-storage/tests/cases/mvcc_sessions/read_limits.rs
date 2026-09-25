//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn bounded_value_reads_preserve_memory_session_and_retained_private_views() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    uqa_storage::key_value::conformance::verify_bounded_value_reads(&store).unwrap();
}

struct Unsupported;

impl CommittedRecordSnapshot for Unsupported {
    fn sequence(&self) -> CommitSequence {
        CommitSequence::INITIAL
    }
    fn get(
        &self,
        _key: &[u8],
        _control: &StorageReadControl,
    ) -> VersionResult<Option<RecordVersion<SharedRecordValue>>> {
        panic!("bounded reads must not fall back to an unbounded materializer");
    }
    fn scan(
        &self,
        _prefix: &[u8],
        _after: Option<&[u8]>,
        _limit: usize,
        _control: &StorageReadControl,
    ) -> VersionResult<RecordPage> {
        panic!("bounded point reads must not scan");
    }
}

#[test]
fn unsupported_bounded_value_reads_do_not_materialize_before_rejection() {
    let control = StorageReadControl::with_limit(4096);
    let snapshot = retain_record_snapshot(Unsupported, &control).unwrap();
    let mut visited = false;
    let error = snapshot
        .visit_value_bounded(b"key", 32, &control, &mut |_| {
            visited = true;
            Ok(())
        })
        .unwrap_err();
    assert!(matches!(
        error,
        VersionError::Storage(StorageBackendError::Other(_))
    ));
    assert!(!visited);
    drop(snapshot);
    assert_eq!(control.memory().used(), 0);
}
