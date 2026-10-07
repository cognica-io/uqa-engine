//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{
    io::{Seek, SeekFrom},
    os::unix::fs::FileExt,
};

use super::*;

#[test]
fn shared_mapping_preserves_sparse_bytes_and_position_and_shares_positioned_io() {
    let mut file = tempfile::tempfile().unwrap();
    let length = 256 * 1024;
    file.set_len(length as u64).unwrap();
    file.write_all_at(b"original", 32 * 1024).unwrap();
    file.seek(SeekFrom::Start(17)).unwrap();
    let mapping = match SharedFileMapping::new(&file, length) {
        Ok(mapping) => mapping,
        Err(error) if error.kind() == io::ErrorKind::Unsupported => return,
        Err(error) => panic!("mapping failed: {error}"),
    };
    assert_eq!(file.stream_position().unwrap(), 17);
    assert_eq!(file.metadata().unwrap().len(), length as u64);
    let mut bytes = [0_u8; 8];
    // SAFETY: this test exclusively owns an unchanged file and both buffers.
    unsafe {
        assert!(mapping.read(32 * 1024, &mut bytes));
        assert_eq!(&bytes, b"original");
        assert!(mapping.read(128 * 1024, &mut bytes));
        assert_eq!(bytes, [0; 8]);
        assert!(mapping.write(128 * 1024, b"mapped!!"));
    }
    file.read_exact_at(&mut bytes, 128 * 1024).unwrap();
    assert_eq!(&bytes, b"mapped!!");
    file.write_all_at(b"changed!", 32 * 1024).unwrap();
    unsafe {
        assert!(mapping.read(32 * 1024, &mut bytes));
        assert_eq!(&bytes, b"changed!");
        assert!(!mapping.read(length as u64 - 4, &mut bytes));
        assert!(!mapping.write(u64::MAX, b"no"));
    }
    drop(mapping);
    file.read_exact_at(&mut bytes, 128 * 1024).unwrap();
    assert_eq!(&bytes, b"mapped!!");
}

#[test]
fn shared_mapping_rejects_nonexistent_extents() {
    let file = tempfile::tempfile().unwrap();
    for length in [0, 1, usize::MAX] {
        assert!(SharedFileMapping::new(&file, length).is_err());
    }
    assert_eq!(file.metadata().unwrap().len(), 0);
}

#[test]
fn an_interrupted_record_copy_leaves_its_unpublished_state() {
    let file = tempfile::tempfile().unwrap();
    file.set_len(4096).unwrap();
    let mapping = match SharedFileMapping::new(&file, 4096) {
        Ok(mapping) => mapping,
        Err(error) if error.kind() == io::ErrorKind::Unsupported => return,
        Err(error) => panic!("mapping failed: {error}"),
    };
    let mut record = [7_u8; 32];
    record[22] = 1;
    INTERRUPT_PUBLICATION.set(true);
    assert!(
        std::panic::catch_unwind(|| unsafe { mapping.write_published(32, &record, 22, 2) })
            .is_err()
    );
    let mut bytes = [0_u8; 32];
    unsafe {
        assert!(mapping.read(32, &mut bytes));
    }
    assert_eq!(bytes[22], 2);
    unsafe {
        assert!(mapping.write_published(32, &record, 22, 2));
    }
    file.read_exact_at(&mut bytes, 32).unwrap();
    assert_eq!(bytes, record);
}
