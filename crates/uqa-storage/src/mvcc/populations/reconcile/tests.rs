//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    diskann_index::format::{DiskANNCanonicalOrigin, DiskANNVectorVersion},
    key_value::KeyValueDiskANNPopulationRecords,
    mvcc::{
        overlay::run::{read_counts, SpilledRunWriter},
        PrivateRecordRevision, StorageTransactionId,
    },
};

fn field(name: &str) -> Vec<u8> {
    let mut key = vec![b'v'];
    for value in ["table", name] {
        key.extend_from_slice(&(value.len() as u32).to_be_bytes());
        key.extend_from_slice(value.as_bytes());
    }
    key
}

#[test]
fn spilled_population_field_reads_seek_and_load_only_the_selected_origins() {
    const FIELDS: usize = 32;
    const DOCUMENTS: usize = 64;
    let control = StorageReadControl::with_limit(128 << 10);
    let layout = KeyValueDiskANNPopulationRecords;
    let identity = StorageTransactionId::new(DatabaseId::from_bytes([7; 16]), 1).unwrap();
    let origin =
        DiskANNCanonicalOrigin::new(DiskANNVectorVersion::new(identity, 1).unwrap(), 128, 1)
            .unwrap();
    let fields: Vec<_> = (0..FIELDS)
        .map(|index| field(&format!("field{index:02}")))
        .collect();
    let mut writer = SpilledRunWriter::new(
        (FIELDS * DOCUMENTS) as u64,
        (FIELDS * DOCUMENTS * 64) as u64,
        control.memory(),
    )
    .unwrap();
    for field in &fields {
        let prefix = layout.origin_prefix(field, &control).unwrap();
        for document in 0..=DOCUMENTS as u64 {
            let mut key = prefix.to_vec();
            key.extend_from_slice(&document.to_be_bytes());
            writer
                .push(
                    &key,
                    None,
                    if document == DOCUMENTS as u64 {
                        RecordWriteKind::Canonical
                    } else {
                        RecordWriteKind::DiskANNOrigin
                    },
                    PrivateRecordRevision::for_tests(),
                    Some(&origin.encode()),
                    &control,
                )
                .unwrap();
        }
    }
    let prepared =
        PreparedRecordCommit::from_spilled_run(writer.finish().unwrap().unwrap(), &control)
            .unwrap();
    assert!(prepared.resident().is_none());
    let index = PreparedLookup::new(&prepared, &control).unwrap();
    read_counts::take();
    // Validation and two population generations each replay only this field's inputs.
    for field in &fields {
        for _ in 0..3 {
            let mut documents = Vec::new();
            visit_field_origins(&layout, &index, field, &control, &mut |change| {
                assert_eq!(&*change.field, field);
                assert_eq!(change.origin, origin);
                documents.push(change.document);
                Ok(())
            })
            .unwrap();
            assert_eq!(documents, (0..DOCUMENTS as u64).collect::<Vec<_>>());
        }
    }
    let reads = read_counts::take();
    assert_eq!(reads.values, 3 * FIELDS * DOCUMENTS, "{reads:?}");
    // Starting a range may decode the preceding part of one 16 KiB entry block and one ending boundary. It must not scan the other 31 fields.
    assert!(reads.entries < 3 * FIELDS * (DOCUMENTS + 256), "{reads:?}");
    control.cancellation().cancel();
    assert!(visit_field_origins(&layout, &index, &fields[0], &control, &mut |_| Ok(())).is_err());
    drop(index);
    drop(prepared);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn resident_population_ranges_distinguish_fields_with_shared_name_prefixes() {
    let control = StorageReadControl::with_limit(128 << 10);
    let layout = KeyValueDiskANNPopulationRecords;
    let identity = StorageTransactionId::new(DatabaseId::from_bytes([8; 16]), 1).unwrap();
    let origin =
        DiskANNCanonicalOrigin::new(DiskANNVectorVersion::new(identity, 1).unwrap(), 3, 1).unwrap();
    let fields = ["a", "aa", "a\0"].map(field);
    let mut writes = BudgetedVec::new(control.memory());
    for field in fields.iter().rev() {
        let mut key = layout.origin_prefix(field, &control).unwrap();
        key.extend_from_slice(&7_u64.to_be_bytes()).unwrap();
        writes
            .push(
                PreparedRecordWrite::copy_bytes(&key, None, Some(&origin.encode()), &control)
                    .unwrap()
                    .with_kind(RecordWriteKind::DiskANNOrigin),
            )
            .unwrap();
    }
    let prepared = PreparedRecordCommit::from_unique_owned(writes, &control).unwrap();
    let index = PreparedLookup::new(&prepared, &control).unwrap();
    for field in fields {
        let mut count = 0;
        visit_field_origins(&layout, &index, &field, &control, &mut |change| {
            assert_eq!(&*change.field, &field);
            count += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 1);
    }
}
