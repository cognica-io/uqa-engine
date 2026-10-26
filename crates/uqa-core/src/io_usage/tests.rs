//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn options() -> IOOptions {
    IOOptions {
        buffers: true,
        wal: true,
    }
}

fn blocks(class: BlockClass, operation: BlockOperation, count: u64) -> IOEvent {
    IOEvent::Blocks {
        class,
        operation,
        count,
        elapsed: Duration::from_micros(7),
    }
}

#[test]
fn actual_events_keep_storage_classes_journal_counts_and_inclusive_ownership() {
    let budget = MemoryBudget::new(4096);
    let parent =
        IOObservation::new(&budget, options(), Arc::new(AtomicBool::new(false)), None).unwrap();
    let child = parent.child(&budget).unwrap();
    child.record(blocks(BlockClass::Shared, BlockOperation::Hit, 2));
    child.record(blocks(BlockClass::Local, BlockOperation::Read, 3));
    child.record(blocks(BlockClass::Local, BlockOperation::Dirty, 5));
    child.record(blocks(BlockClass::Temporary, BlockOperation::Write, 7));
    let wal = WALUsage {
        records: 11,
        full_page_images: 13,
        bytes: 17,
        buffers_full: 19,
    };
    child.record(IOEvent::WAL(wal));
    let observed = child.finish().unwrap();
    assert_eq!(observed.shared.hits, 2);
    assert_eq!(observed.local.reads, 3);
    assert_eq!(observed.local.dirtied, 5);
    assert_eq!(observed.local.read_time, Duration::from_micros(7));
    assert_eq!(observed.temporary.written, 7);
    assert_eq!(observed.temporary.write_time, Duration::from_micros(7));
    assert_eq!(observed.wal, wal);
    assert_eq!(parent.snapshot().unwrap(), observed);
    child.record(blocks(BlockClass::Shared, BlockOperation::Hit, 101));
    assert_eq!(
        parent.snapshot().unwrap(),
        observed,
        "closed requests cannot charge their ancestor"
    );
    parent.record(blocks(BlockClass::Shared, BlockOperation::Hit, 1));
    assert_eq!(parent.finish().unwrap().shared.hits, 3);
    drop(child);
    drop(parent);
    assert_eq!(budget.used(), 0);
}

#[test]
fn explicit_thread_bindings_and_parallel_children_exclude_unrelated_work() {
    let budget = MemoryBudget::new(8192);
    let root =
        IOObservation::new(&budget, options(), Arc::new(AtomicBool::new(false)), None).unwrap();
    let unrelated =
        IOObservation::new(&budget, options(), Arc::new(AtomicBool::new(false)), None).unwrap();
    let first = root.child(&budget).unwrap();
    let second = root.child(&budget).unwrap();
    std::thread::scope(|threads| {
        for (child, count) in [(&first, 17), (&second, 31)] {
            let unrelated = &unrelated;
            threads.spawn(move || {
                let _bound = IOContext::new(Some(child.clone())).enter();
                for _ in 0..count {
                    IOContext::record(blocks(BlockClass::Shared, BlockOperation::Read, 1));
                }
                {
                    let _other = IOContext::new(Some(unrelated.clone())).enter();
                    IOContext::record(blocks(BlockClass::Shared, BlockOperation::Read, 1));
                }
                let _absent = IOContext::default().enter();
                IOContext::record(blocks(BlockClass::Shared, BlockOperation::Read, 999));
            });
        }
    });
    assert_eq!(first.snapshot().unwrap().shared.reads, 17);
    assert_eq!(second.snapshot().unwrap().shared.reads, 31);
    assert_eq!(root.snapshot().unwrap().shared.reads, 48);
    assert_eq!(unrelated.snapshot().unwrap().shared.reads, 2);
    assert!(IOContext::current().observation().is_none());
}

#[test]
fn suspended_worker_replacement_and_unwind_restore_the_invoking_context() {
    let budget = MemoryBudget::new(4096);
    let first =
        IOObservation::new(&budget, options(), Arc::new(AtomicBool::new(false)), None).unwrap();
    let second =
        IOObservation::new(&budget, options(), Arc::new(AtomicBool::new(false)), None).unwrap();
    let initial = IOContext::new(Some(first.clone()));
    let _outer = initial.enter();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let worker = IOContext::default().enter();
        worker.replace(&IOContext::new(Some(second.clone())));
        IOContext::record(blocks(BlockClass::Temporary, BlockOperation::Read, 1));
        worker.replace(&IOContext::default());
        IOContext::record(blocks(BlockClass::Temporary, BlockOperation::Read, 999));
        panic!("unwind worker");
    }));
    assert!(failed.is_err());
    IOContext::record(blocks(BlockClass::Temporary, BlockOperation::Write, 1));
    assert_eq!(first.snapshot().unwrap().temporary.reads, 0);
    assert_eq!(first.snapshot().unwrap().temporary.written, 1);
    assert_eq!(second.snapshot().unwrap().temporary.reads, 1);
    assert_eq!(second.snapshot().unwrap().temporary.written, 0);
}

#[test]
fn disabled_observation_and_failed_io_add_no_completed_operation() {
    let budget = MemoryBudget::new(4096);
    let root = IOObservation::new(
        &budget,
        IOOptions {
            buffers: true,
            ..IOOptions::default()
        },
        Arc::new(AtomicBool::new(false)),
        None,
    )
    .unwrap();
    let _scope = IOContext::new(Some(root.clone())).enter();
    let completed =
        IOContext::block_operation(BlockClass::Shared, BlockOperation::Read, 1).unwrap();
    assert!(
        completed.started.is_none(),
        "disabled I/O timing must not read the clock"
    );
    completed.complete();
    drop(IOContext::block_operation(
        BlockClass::Local,
        BlockOperation::Write,
        2,
    ));
    root.record(IOEvent::WAL(WALUsage {
        bytes: 1,
        ..WALUsage::default()
    }));
    let usage = root.snapshot().unwrap();
    assert_eq!(usage.shared.reads, 1);
    assert_eq!(usage.shared.read_time, Duration::ZERO);
    assert_eq!(usage.local.written, 0);
    assert_eq!(usage.wal, WALUsage::default());
    let _absent = IOContext::default().enter();
    assert!(IOContext::block_operation(BlockClass::Temporary, BlockOperation::Read, 1).is_none());
}

#[test]
fn child_switches_do_not_hide_events_requested_by_an_enclosing_invocation() {
    let budget = MemoryBudget::new(4096);
    let parent =
        IOObservation::new(&budget, options(), Arc::new(AtomicBool::new(false)), None).unwrap();
    let child = IOObservation::new(
        &budget,
        IOOptions::default(),
        Arc::new(AtomicBool::new(false)),
        Some(parent.clone()),
    )
    .unwrap();
    let _scope = IOContext::new(Some(child.clone())).enter();
    IOContext::block_operation(BlockClass::Temporary, BlockOperation::Read, 1)
        .unwrap()
        .complete();
    IOContext::record(IOEvent::WAL(WALUsage {
        records: 1,
        ..WALUsage::default()
    }));
    assert_eq!(child.snapshot().unwrap(), IOUsage::default());
    assert_eq!(parent.snapshot().unwrap().temporary.reads, 1);
    assert_eq!(parent.snapshot().unwrap().wal.records, 1);
}

#[test]
fn overflow_is_terminal_and_never_publishes_truncated_counters() {
    for event in [
        blocks(BlockClass::Shared, BlockOperation::Hit, u64::MAX),
        IOEvent::WAL(WALUsage {
            bytes: u64::MAX,
            ..WALUsage::default()
        }),
        IOEvent::Blocks {
            class: BlockClass::Local,
            operation: BlockOperation::Read,
            count: 1,
            elapsed: Duration::MAX,
        },
    ] {
        let budget = MemoryBudget::new(4096);
        let observed =
            IOObservation::new(&budget, options(), Arc::new(AtomicBool::new(false)), None).unwrap();
        observed.record(event);
        assert!(observed.snapshot().is_ok());
        observed.record(event);
        assert!(observed.snapshot().is_err());
        assert!(observed.finish().is_err());
    }
}

#[test]
fn owner_admission_precedes_allocation_and_events_allocate_nothing() {
    let rejected = MemoryBudget::new(0);
    let timing = Arc::new(AtomicBool::new(false));
    let allocation = allocation_counter::measure(|| {
        assert!(IOObservation::new(&rejected, options(), Arc::clone(&timing), None).is_err());
    });
    assert_eq!(allocation.bytes_total, 0);
    assert_eq!(rejected.used(), 0);
    let budget = MemoryBudget::new(4096);
    let observed =
        IOObservation::new(&budget, options(), Arc::new(AtomicBool::new(false)), None).unwrap();
    assert!(budget.used() > 0);
    let _scope = IOContext::new(Some(observed.clone())).enter();
    let allocation = allocation_counter::measure(|| {
        for _ in 0..1000 {
            IOContext::record(blocks(BlockClass::Shared, BlockOperation::Hit, 1));
        }
    });
    assert_eq!(allocation.bytes_total, 0);
    assert_eq!(observed.snapshot().unwrap().shared.hits, 1000);
}

#[test]
fn timing_changes_apply_at_operation_start_and_parent_keeps_measured_durations() {
    use std::sync::atomic::Ordering;
    let budget = MemoryBudget::new(4096);
    let timing = Arc::new(AtomicBool::new(false));
    let parent = IOObservation::new(&budget, options(), Arc::clone(&timing), None).unwrap();
    let child = parent.child(&budget).unwrap();
    let _scope = IOContext::new(Some(child.clone())).enter();
    let operation =
        || IOContext::block_operation(BlockClass::Temporary, BlockOperation::Read, 1).unwrap();
    let before = operation();
    assert!(before.started.is_none());
    timing.store(true, Ordering::Release);
    before.complete();
    assert_eq!(
        parent.snapshot().unwrap().temporary.read_time,
        Duration::ZERO
    );
    let mut during = operation();
    let started = during.started.expect("enabled operation reads the clock");
    // Keep the synthetic interval positive even on platforms with coarse clock resolution.
    during.started = Some(started.checked_sub(Duration::from_millis(1)).unwrap());
    timing.store(false, Ordering::Release);
    during.complete();
    let measured = parent.snapshot().unwrap();
    assert!(measured.temporary.read_time > Duration::ZERO);
    let after = operation();
    assert!(after.started.is_none());
    after.complete();
    assert_eq!(
        parent.snapshot().unwrap().temporary.read_time,
        measured.temporary.read_time
    );
    assert_eq!(child.finish().unwrap(), parent.finish().unwrap());
}

#[test]
fn selecting_an_invocation_keeps_only_its_own_active_child() {
    let budget = MemoryBudget::new(4096);
    let timing = Arc::new(AtomicBool::new(false));
    let parent = IOObservation::new(&budget, options(), Arc::clone(&timing), None).unwrap();
    let child = parent.child(&budget).unwrap();
    let unrelated = IOObservation::new(&budget, options(), timing, None).unwrap();
    let _scope = IOContext::new(Some(child.clone())).enter();
    assert!(parent.is_within(&parent));
    assert!(child.is_within(&parent));
    assert!(!parent.is_within(&child));
    assert!(IOContext::for_observation(Some(&parent))
        .observation()
        .unwrap()
        .is_within(&child));
    assert!(IOContext::for_observation(Some(&unrelated))
        .observation()
        .unwrap()
        .is_within(&unrelated));
    assert!(IOContext::for_observation(None).observation().is_none());
}
