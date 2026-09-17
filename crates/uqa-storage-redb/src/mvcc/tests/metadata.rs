//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained record owners reject incompatible formats and replaced database incarnations.

use super::*;
use uqa_storage::mvcc::{IdentifierRequest, RecordWrite};

fn rejected<T>(result: VersionResult<T>, replaced: bool) {
    let error = result.err().expect("incompatible metadata was accepted");
    if replaced {
        assert!(matches!(error, VersionError::WrongDatabase), "{error}");
    } else {
        assert!(matches!(error, VersionError::InvalidEncoding(_)), "{error}");
    }
}

fn reject_snapshot_access(
    retained: &dyn CommittedRecordSnapshot,
    control: &StorageReadControl,
    replaced: bool,
) {
    rejected(retained.get(b"a", control), replaced);
    rejected(retained.scan(b"", None, 4, control), replaced);
    let mut visited = false;
    rejected(
        retained.visit_value(b"a", control, &mut |_| {
            visited = true;
            Ok(())
        }),
        replaced,
    );
    assert!(!visited);
    rejected(
        retained.visit_prefix(b"", None, 4, control, &mut |_, _| {
            visited = true;
            Ok(true)
        }),
        replaced,
    );
    assert!(!visited);
}

#[test]
fn retained_records_and_snapshots_revalidate_format_and_incarnation_before_access() {
    for (field, replacement) in [
        ("format", 9_u64.to_be_bytes().to_vec()),
        ("format", 99_u64.to_be_bytes().to_vec()),
        ("database", vec![0x7f; 16]),
    ] {
        let replaced = field == "database";
        let database = Arc::new(
            Database::builder()
                .create_with_backend(InMemoryBackend::new())
                .unwrap(),
        );
        let store = RedbRecordStore::new(database.clone()).unwrap();
        store.migrate_key_value().unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let batch = |key: &[u8]| {
            PreparedRecordCommit::new(
                &[RecordWrite {
                    key,
                    expected: None,
                    value: Some(b"original"),
                }],
                &control,
            )
            .unwrap()
        };
        let initial = batch(b"a");
        let first = store.allocate_transaction(&control).unwrap();
        let receipt = store.commit(first, &initial, &control).unwrap();
        let pending = store.allocate_transaction(&control).unwrap();
        let next = batch(b"b");
        let retained = store.snapshot(&control).unwrap();
        store
            .allocate_identifiers(b"rows", IdentifierRequest::Observe(7), &control)
            .unwrap();
        let old = {
            let transaction = physical_writer(&database).unwrap();
            let old = {
                let mut metadata = transaction.open_table(METADATA).unwrap();
                let old = metadata.get(field).unwrap().unwrap().value().to_vec();
                metadata.insert(field, replacement.as_slice()).unwrap();
                old
            };
            transaction.commit().unwrap();
            old
        };

        rejected(store.snapshot(&control), replaced);
        rejected(store.allocate_transaction(&control), replaced);
        rejected(store.commit_status(first, &control), replaced);
        rejected(store.abort(pending, &control), replaced);
        rejected(store.migrate_key_value(), replaced);
        rejected(store.identifier_watermark(b"rows", &control), replaced);
        rejected(
            store.allocate_identifiers(b"rows", IdentifierRequest::Observe(8), &control),
            replaced,
        );
        for (id, writes) in [(first, &initial), (pending, &next)] {
            let Err(CommitFailure::Rejected(error)) = store.commit(id, writes, &control) else {
                panic!("a commit accepted incompatible metadata");
            };
            rejected::<()>(Err(error), replaced);
        }
        reject_snapshot_access(&*retained, &control, replaced);

        let transaction = physical_writer(&database).unwrap();
        {
            let mut metadata = transaction.open_table(METADATA).unwrap();
            assert_eq!(
                read_u64(&metadata, "allocated").unwrap(),
                pending.allocation()
            );
            assert_eq!(
                read_u64(&metadata, "sequence").unwrap(),
                receipt.sequence.as_u64()
            );
            metadata.insert(field, old.as_slice()).unwrap();
        }
        transaction.commit().unwrap();
        assert_eq!(
            store.commit_status(first, &control).unwrap(),
            CommitStatus::Committed(receipt)
        );
        assert_eq!(
            store.commit_status(pending, &control).unwrap(),
            CommitStatus::Pending
        );
        assert_eq!(
            store.identifier_watermark(b"rows", &control).unwrap(),
            Some(7)
        );
        assert!(retained.get(b"b", &control).unwrap().is_none());
        assert_eq!(store.commit(first, &initial, &control).unwrap(), receipt);
        store.commit(pending, &next, &control).unwrap();
    }
}
