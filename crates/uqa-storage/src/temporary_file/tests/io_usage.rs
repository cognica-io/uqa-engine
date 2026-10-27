//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::temporary_file::BlockTemporaryFile;
use std::io::{Read, Seek, SeekFrom, Write};
use uqa_core::{
    io_usage::{IOContext, IOObservation, IOOptions, IOUsage},
    memory::MemoryBudget,
};

fn observation(budget: &MemoryBudget) -> IOObservation {
    IOObservation::new(
        budget,
        IOOptions {
            buffers: true,
            ..IOOptions::default()
        },
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        None,
    )
    .unwrap()
}

#[test]
fn temporary_counts_follow_actual_block_reads_and_publications() {
    let budget = MemoryBudget::new(4096);
    let observed = observation(&budget);
    let _scope = IOContext::new(Some(observed.clone())).enter();
    let mut file = BlockTemporaryFile::<4>::new().unwrap();
    file.write_all(b"abcdefgh").unwrap();
    let first = observed.snapshot().unwrap();
    assert_eq!(first.temporary.written, 2);
    assert_eq!(
        first.temporary.reads, 0,
        "new zero-filled blocks perform no read"
    );
    file.seek(SeekFrom::Start(1)).unwrap();
    file.write_all(b"XY").unwrap();
    let mut reader = file.reopen().unwrap();
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"aXYdefgh");
    assert_eq!(reader.read(&mut bytes).unwrap(), 0);
    file.flush().unwrap();
    let final_usage = observed.finish().unwrap();
    assert_eq!(final_usage.temporary.reads, 3);
    assert_eq!(final_usage.temporary.written, 3);
    assert_eq!(final_usage.temporary.read_time, std::time::Duration::ZERO);
    assert_eq!(final_usage.temporary.write_time, std::time::Duration::ZERO);
    assert_eq!(final_usage.shared, IOUsage::default().shared);
    assert_eq!(final_usage.local, IOUsage::default().local);
    assert_eq!(final_usage.wal, IOUsage::default().wal);
}

#[test]
fn failed_publication_does_not_claim_a_completed_block_write() {
    let budget = MemoryBudget::new(4096);
    let observed = observation(&budget);
    let _scope = IOContext::new(Some(observed.clone())).enter();
    let mut file = BlockTemporaryFile::<4>::new().unwrap();
    file.write_all(b"abcd").unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.fail_write_after(0);
    assert!(file.write_all(b"WXYZ").is_err());
    assert_eq!(observed.snapshot().unwrap().temporary.reads, 1);
    assert_eq!(observed.snapshot().unwrap().temporary.written, 1);
    let mut reader = file.reopen().unwrap();
    let mut retained = [0; 4];
    reader.read_exact(&mut retained).unwrap();
    assert_eq!(&retained, b"abcd");
}

#[test]
fn retained_readers_charge_the_executing_request_instead_of_the_file_creator() {
    let budget = MemoryBudget::new(4096);
    let first = observation(&budget);
    let second = observation(&budget);
    let mut file = BlockTemporaryFile::<4>::new().unwrap();
    {
        let _scope = IOContext::new(Some(first.clone())).enter();
        file.write_all(b"abcdefgh").unwrap();
    }
    let mut reader = file.reopen().unwrap();
    {
        let _scope = IOContext::new(Some(second.clone())).enter();
        let mut bytes = [0; 4];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"abcd");
    }
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"efgh");
    assert_eq!(first.finish().unwrap().temporary.written, 2);
    assert_eq!(first.snapshot().unwrap().temporary.reads, 0);
    assert_eq!(second.finish().unwrap().temporary.reads, 1);
    assert_eq!(second.snapshot().unwrap().temporary.written, 0);
}
