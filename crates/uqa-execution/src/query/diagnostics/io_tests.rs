//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use uqa_storage::temporary_file::BlockTemporaryFile;

fn options() -> IOOptions {
    IOOptions {
        buffers: true,
        wal: false,
    }
}

fn write_blocks(count: usize) -> BlockTemporaryFile<4> {
    let mut file = BlockTemporaryFile::new().unwrap();
    for _ in 0..count {
        file.write_all(b"abcd").unwrap();
    }
    file
}

#[test]
fn nested_invocations_count_real_io_once_and_keep_options_independent() {
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let outer = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    write_blocks(2);
    let inner = owner
        .enter_measured(control.clone(), false, IOOptions::default())
        .unwrap();
    write_blocks(3);
    assert_eq!(inner.finish_execution().unwrap().io, IOUsage::default());
    let inner = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    write_blocks(5);
    assert_eq!(inner.finish_execution().unwrap().io.temporary.written, 5);
    write_blocks(7);
    assert_eq!(outer.finish_execution().unwrap().io.temporary.written, 17);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn unrelated_session_on_the_same_thread_does_not_inherit_io() {
    let owner = QueryDiagnostics::default();
    let independent = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let outer = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    write_blocks(2);
    {
        let _statement = independent.io_context().enter();
        write_blocks(11);
        let other = independent
            .enter_measured(control.clone(), false, options())
            .unwrap();
        write_blocks(3);
        assert_eq!(other.finish_execution().unwrap().io.temporary.written, 3);
        write_blocks(13);
    }
    write_blocks(5);
    assert_eq!(outer.finish_execution().unwrap().io.temporary.written, 7);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn parallel_workers_keep_the_parent_and_live_session_clock() {
    let timing = Arc::new(AtomicBool::new(false));
    let owner = QueryDiagnostics::new(Arc::clone(&timing));
    let control = StorageReadControl::with_limit(65_536);
    let outer = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    write_blocks(1);
    assert_eq!(
        outer
            .collector
            .io
            .as_ref()
            .unwrap()
            .snapshot()
            .unwrap()
            .temporary
            .write_time,
        std::time::Duration::ZERO
    );
    let worker = owner.fork();
    let capture = owner.capture().unwrap();
    timing.store(true, Ordering::Release);
    std::thread::scope(|threads| {
        for count in [2, 3, 5, 7] {
            let worker = &worker;
            let capture = &capture;
            let control = &control;
            threads.spawn(move || {
                let _bound = worker.bind(capture).unwrap();
                let inner = worker
                    .enter_measured(control.clone(), false, options())
                    .unwrap();
                write_blocks(count);
                let actual = inner.finish_execution().unwrap();
                assert_eq!(actual.io.temporary.written, count as u64);
                assert!(actual.io_timing);
                assert!(actual.io.temporary.write_time > std::time::Duration::ZERO);
            });
        }
    });
    timing.store(false, Ordering::Release);
    let actual = outer.finish_execution().unwrap();
    assert!(!actual.io_timing);
    assert_eq!(actual.io.temporary.written, 18);
    assert!(actual.io.temporary.write_time > std::time::Duration::ZERO);
    drop((actual, worker, capture));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_parallel_dispatch_carries_io_and_clears_it_for_unobserved_work() {
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let outer = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    crate::parallel::run_parallel(vec![1, 2, 3, 4], 4, |chunk| {
        write_blocks(chunk.len());
        chunk
    });
    let write = || {
        write_blocks(1);
    };
    crate::parallel::ParallelExecutor::new(4).execute_branches(&[write; 4]);
    let retained = owner.capture().unwrap();
    assert_eq!(outer.finish_execution().unwrap().io.temporary.written, 8);
    crate::parallel::ParallelExecutor::new(4).execute_branches(&[write; 4]);
    assert_eq!(
        retained
            .io
            .as_ref()
            .unwrap()
            .snapshot()
            .unwrap()
            .temporary
            .written,
        8
    );
    drop(retained);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn retained_reader_rebinds_each_request_including_absence_and_closed_capture() {
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let file = write_blocks(4);
    let mut reader = file.reopen().unwrap();
    let first = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    let stale = owner.request();
    let worker = owner.fork();
    let request = owner.request();
    let read = |reader: &mut BlockTemporaryFile<4>| {
        let mut bytes = [0; 4];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"abcd");
    };
    std::thread::scope(|threads| {
        threads
            .spawn(|| {
                let _request = QueryDiagnostics::request_scope(Some(request));
                let _statement = worker.io_context().enter();
                read(&mut reader);
            })
            .join()
            .unwrap();
    });
    assert_eq!(first.finish_execution().unwrap().io.temporary.reads, 1);
    let second = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    let request = owner.request();
    std::thread::scope(|threads| {
        threads
            .spawn(|| {
                let request_scope = QueryDiagnostics::request_scope(Some(stale));
                read(&mut reader);
                request_scope.replace(Some(request));
                read(&mut reader);
                request_scope.replace(None);
                read(&mut reader);
            })
            .join()
            .unwrap();
    });
    assert_eq!(second.finish_execution().unwrap().io.temporary.reads, 1);
    drop(worker);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn cancelled_and_unwound_observations_release_and_restore_their_owners() {
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let outer = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _inner = owner
            .enter_measured(control.clone(), false, options())
            .unwrap();
        write_blocks(1);
        panic!("host callback failed");
    }));
    assert!(caught.is_err());
    write_blocks(2);
    assert_eq!(outer.finish_execution().unwrap().io.temporary.written, 3);
    assert_eq!(control.memory().used(), 0);
    let scope = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    control.cancellation().cancel();
    assert_eq!(
        scope.finish_execution().err().unwrap().sqlstate(),
        Some("57014")
    );
    assert_eq!(control.memory().used(), 0);
    assert!(IOContext::current().observation().is_none());
    assert!(owner
        .enter_measured(StorageReadControl::with_limit(0), false, options())
        .is_err());
    assert!(owner.capture().is_none());
}
