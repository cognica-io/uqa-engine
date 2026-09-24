//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Interrupted slot writes and length changes preserve the published prefix for every reader.

use super::*;

fn contents<const BYTES: usize>(file: &BlockTemporaryFile<BYTES>) -> Vec<u8> {
    let mut bytes = Vec::new();
    file.reopen().unwrap().read_to_end(&mut bytes).unwrap();
    bytes
}

fn rejected_writes<const BYTES: usize>() {
    for append in [false, true] {
        let populated = if append { 5 } else { BYTES };
        for failed_after in [
            0,
            NONCE_BYTES / 2,
            NONCE_BYTES,
            NONCE_BYTES + LENGTH_BYTES - 1,
            NONCE_BYTES + LENGTH_BYTES + TAG_BYTES / 2,
            SLOT_HEADER_BYTES + populated / 2,
            SLOT_HEADER_BYTES + populated - 1,
            SLOT_HEADER_BYTES + populated,
        ] {
            let mut file = BlockTemporaryFile::<BYTES>::new().unwrap();
            let mut expected = vec![b'a'; BYTES * 2 + 3];
            file.write_all(&expected).unwrap();
            let mut retained = file.reopen().unwrap();
            let position = if append { expected.len() } else { BYTES + 2 };
            file.seek(SeekFrom::Start(position as u64)).unwrap();
            file.owner.lock().faults.fail_after_bytes = Some(failed_after);
            assert!(file.write(b"xy").is_err());
            assert_eq!(file.stream_position().unwrap(), position as u64);
            assert_eq!(file.metadata().unwrap().len(), expected.len() as u64);
            assert_eq!(contents(&file), expected);
            let mut restored = Vec::new();
            retained.read_to_end(&mut restored).unwrap();
            assert_eq!(restored, expected);
            file.write_all(b"xy").unwrap();
            if append {
                expected.extend_from_slice(b"xy");
            } else {
                expected[position..position + 2].copy_from_slice(b"xy");
            }
            assert_eq!(contents(&file), expected);
        }
    }
}

#[test]
fn failed_append_and_overwrite_preserve_the_prior_slot_and_independent_readers() {
    rejected_writes::<8>();
    rejected_writes::<BLOCK_BYTES>();
}

#[test]
fn failed_new_blocks_and_sparse_growth_remove_unpublished_tails() {
    for sparse in [false, true] {
        let mut file = BlockTemporaryFile::<8>::new().unwrap();
        file.write_all(b"retained").unwrap();
        let position = if sparse { 8 * 3 + 2 } else { 8 };
        file.seek(SeekFrom::Start(position)).unwrap();
        // Sparse growth completes one new block before the next partial write fails.
        file.owner.lock().faults.fail_after_bytes = Some(if sparse { 51 + 30 } else { 30 });
        assert!(file.write_all(b"new").is_err());
        assert_eq!(file.metadata().unwrap().len(), 8);
        assert_eq!(std::fs::metadata(file.path()).unwrap().len(), 101);
        assert_eq!(contents(&file), b"retained");
        file.write_all(b"new").unwrap();
        let mut expected = b"retained".to_vec();
        expected.resize(position as usize, 0);
        expected.extend_from_slice(b"new");
        assert_eq!(contents(&file), expected);
    }
}

#[test]
fn failed_truncation_preserves_bytes_and_growth_never_reveals_the_truncated_tail() {
    let mut file = BlockTemporaryFile::<8>::new().unwrap();
    let original = b"retained-sensitive-truncated-tail";
    file.write_all(original).unwrap();
    let written = file.owner.lock().faults.written_bytes;
    file.owner.lock().faults.fail_truncate = true;
    assert!(file.set_len(11).is_err());
    assert_eq!(file.metadata().unwrap().len(), original.len() as u64);
    assert_eq!(contents(&file), original);
    file.set_len(11).unwrap();
    assert_eq!(file.owner.lock().faults.written_bytes, written);
    assert_eq!(contents(&file), &original[..11]);
    file.owner.lock().faults.fail_after_bytes = Some(51 + 30);
    assert!(file.set_len(original.len() as u64).is_err());
    assert_eq!(file.metadata().unwrap().len(), 11);
    assert_eq!(contents(&file), &original[..11]);
    file.set_len(original.len() as u64).unwrap();
    let mut expected = original[..11].to_vec();
    expected.resize(original.len(), 0);
    assert_eq!(contents(&file), expected);
}

#[test]
fn failed_physical_reservation_never_publishes_a_new_block() {
    let mut file = BlockTemporaryFile::<8>::new().unwrap();
    file.write_all(b"retained").unwrap();
    file.owner.lock().faults.fail_truncate = true;
    assert!(file.write(b"x").is_err());
    assert_eq!(file.metadata().unwrap().len(), 8);
    assert_eq!(contents(&file), b"retained");
    file.write_all(b"x").unwrap();
    assert_eq!(contents(&file), b"retainedx");
}

#[test]
fn two_file_append_rollback_keeps_old_rows_after_data_or_offset_failure() {
    for data_fails in [false, true] {
        let mut data = BlockTemporaryFile::<16>::new().unwrap();
        let mut offsets = BlockTemporaryFile::<8>::new().unwrap();
        data.write_all(b"previous-row").unwrap();
        offsets.write_all(&0_u64.to_le_bytes()).unwrap();
        let old_data = data.metadata().unwrap().len();
        let old_offsets = offsets.metadata().unwrap().len();
        let mut retained = data.reopen().unwrap();
        if data_fails {
            // The first partial data block succeeds before the next block's ciphertext fails.
            data.owner.lock().faults.fail_after_bytes = Some(59 + 30);
        } else {
            offsets.owner.lock().faults.fail_after_bytes = Some(30);
        }
        let append = data
            .write_all_vectored(&mut [
                IoSlice::new(b"new-row-"),
                IoSlice::new(b"crosses-several-authenticated-blocks"),
            ])
            .and_then(|()| offsets.write_all(&old_data.to_le_bytes()));
        assert!(append.is_err());
        data.set_len(old_data).unwrap();
        offsets.set_len(old_offsets).unwrap();
        let mut restored = Vec::new();
        retained.read_to_end(&mut restored).unwrap();
        assert_eq!(restored, b"previous-row");
        assert_eq!(contents(&data), b"previous-row");
        assert_eq!(contents(&offsets), 0_u64.to_le_bytes());
        data.seek(SeekFrom::End(0)).unwrap();
        offsets.seek(SeekFrom::End(0)).unwrap();
        data.write_all(b"retry").unwrap();
        offsets.write_all(&old_data.to_le_bytes()).unwrap();
        assert_eq!(contents(&data), b"previous-rowretry");
        assert_eq!(&contents(&offsets)[8..], &old_data.to_le_bytes());
    }
}

#[test]
fn rollback_failure_is_reported_without_publishing_partial_bytes_to_existing_readers() {
    let mut file = BlockTemporaryFile::<8>::new().unwrap();
    file.write_all(b"retained").unwrap();
    let mut retained = file.reopen().unwrap();
    {
        let mut owner = file.owner.lock();
        owner.faults.fail_after_bytes = Some(30);
        owner.faults.fail_truncate = true;
    }
    let error = file.write(b"x").unwrap_err();
    assert!(error.to_string().contains("rollback failed"));
    assert_eq!(file.metadata().unwrap().len(), 8);
    assert!(file.reopen().is_err());
    let mut bytes = Vec::new();
    retained.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"retained");
    file.write_all(b"x").unwrap();
    assert_eq!(contents(&file), b"retainedx");
}
