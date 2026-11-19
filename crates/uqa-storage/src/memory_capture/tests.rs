//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{collections::BTreeMap, sync::atomic::AtomicUsize, sync::mpsc, thread};
use uqa_core::{memory::MemoryError, Value};

mod admission;
mod keys;
mod selection;

thread_local! {
    static ON_SOURCE_LOCKED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static ON_SOURCE_WAIT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn source_locked() {
    if let Some(observe) = ON_SOURCE_LOCKED.take() {
        observe();
    }
}

pub(super) fn source_wait() {
    if let Some(observe) = ON_SOURCE_WAIT.take() {
        observe();
    }
}

struct Scalar {
    value: u64,
    calls: Arc<AtomicUsize>,
    scratch: usize,
}

impl MemoryCaptureSource for Scalar {
    type Frozen = u64;
    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<u64> {
        control.check()?;
        self.calls.fetch_add(1, Ordering::Relaxed);
        let _scratch = control.memory().reserve(self.scratch)?;
        Ok(self.value)
    }
}

fn scalar(registry: &MemoryCaptureRegistry, value: u64) -> MemoryCaptureCell<Scalar> {
    registry
        .cell(Scalar {
            value,
            calls: Arc::default(),
            scratch: 0,
        })
        .unwrap()
}

#[test]
fn captured_memory_values_keep_their_original_allowance_and_cancellation() {
    let control = StorageReadControl::with_limit(4096);
    let foreign = StorageReadControl::with_limit(4096);
    let payload = || {
        let memory = control.memory().reserve(1024).unwrap();
        Budgeted::new(vec![7_u8; 1024], memory)
    };
    assert!(CapturedMemoryValue::new(payload(), &foreign).is_err());
    assert_eq!(control.memory().used(), 0);
    let value = CapturedMemoryValue::new(payload(), &control).unwrap();
    assert_eq!(value.get().unwrap().as_slice(), &[7_u8; 1024]);
    assert_eq!(control.memory().used(), 1024);
    control.cancellation().cancel();
    assert!(value.get().is_err());
    drop(value);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_capture_keys_preserve_retired_sources_without_retaining_live_owners() {
    let registry = MemoryCaptureRegistry::new();
    let cell = scalar(&registry, 7);
    let key = cell.key().unwrap();
    let control = StorageReadControl::with_limit(4096);
    let cut = registry.capture(&control).unwrap();
    assert!(key.retained(&cut).unwrap().is_none());
    let foreign = MemoryCaptureRegistry::new().capture(&control).unwrap();
    assert!(key.retained(&foreign).is_err());
    drop(foreign);
    let read = cell.read();
    assert!(cell.try_write().is_none());
    assert!(key.retained(&cut).unwrap().is_none());
    assert_eq!(read.calls.load(Ordering::Relaxed), 0);
    drop(read);
    cell.try_write().unwrap().value = 8;
    let frozen = key.retained(&cut).unwrap().unwrap();
    assert_eq!(**frozen, 7);
    let later = registry.capture(&control).unwrap();
    let late = registry
        .cell(Scalar {
            value: 9,
            calls: Arc::default(),
            scratch: 0,
        })
        .unwrap();
    assert!(late.key().unwrap().retained(&later).is_err());
    drop(late);
    drop(cell);
    drop(registry);
    assert_eq!(**key.retained(&later).unwrap().unwrap(), 8);
    assert!(Arc::ptr_eq(&key.retained(&cut).unwrap().unwrap(), &frozen));
    drop(later);
    drop(cut);
    assert!(control.memory().used() > 0);
    drop(frozen);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn registry_retirement_releases_registration_without_releasing_captured_values() {
    let registry = MemoryCaptureRegistry::new();
    let cell = scalar(&registry, 7);
    let key = cell.key().unwrap();
    let control = StorageReadControl::with_limit(4096);
    let cut = registry.capture(&control).unwrap();
    let frozen = cell.captured(&cut).unwrap();
    let admitted = control.memory().used();
    drop(cell);
    assert_eq!(control.memory().used(), admitted);
    drop(registry);
    assert!(key.registry.upgrade().is_none());
    assert_eq!(
        control.memory().used(),
        admitted - OwnedMap::<u64, Weak<Capture>>::entry_bytes()
    );
    cut.check().unwrap();
    assert!(Arc::ptr_eq(&frozen, &key.captured(&cut).unwrap()));
    assert_eq!(**frozen, 7);
    drop(cut);
    assert_eq!(control.memory().used(), size_of::<Budgeted<u64>>());
    drop(frozen);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_capture_source_retirement_observes_original_limits_and_cancellation() {
    let registry = MemoryCaptureRegistry::new();
    let cell = scalar(&registry, 7);
    let key = cell.key().unwrap();
    let small = StorageReadControl::with_limit(1024);
    let good = StorageReadControl::with_limit(8192);
    let limited = registry.capture(&small).unwrap();
    let saved = registry.capture(&good).unwrap();
    cell.value().write().scratch = 2048;
    drop(cell);
    assert!(key.retained(&limited).is_err());
    assert_eq!(**key.retained(&saved).unwrap().unwrap(), 7);
    assert!(limited.check().is_err());
    let cancelled = scalar(&registry, 8);
    let cancelled_key = cancelled.key().unwrap();
    let cancellation = StorageReadControl::with_limit(8192);
    let cancelled_cut = registry.capture(&cancellation).unwrap();
    cancellation.cancellation().cancel();
    drop(cancelled);
    assert!(cancelled_key.retained(&cancelled_cut).is_err());
    assert_eq!(**key.retained(&saved).unwrap().unwrap(), 7);
    drop((limited, saved, cancelled_cut));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
    assert_eq!(cancellation.memory().used(), 0);
}

#[test]
fn memory_capture_unregistered_owners_allocate_no_domain_and_join_once() {
    let calls = Arc::default();
    let mut cell = None;
    let allocated = allocation_counter::measure(|| {
        cell = Some(MemoryCaptureCell::new(Scalar {
            value: 1,
            calls,
            scratch: 0,
        }));
    });
    assert_eq!(allocated.bytes_total, 0);
    let mut cell = cell.unwrap();
    let written = allocation_counter::measure(|| cell.write().value = 2);
    assert_eq!(written.bytes_total, 0);
    assert_eq!(cell.read().calls.load(Ordering::Relaxed), 0);
    let registry = MemoryCaptureRegistry::new();
    let control = StorageReadControl::with_limit(4096);
    let before = registry.capture(&control).unwrap();
    assert!(cell.captured(&before).is_err());
    cell.register(&registry).unwrap();
    assert!(cell.captured(&before).is_err());
    let after = registry.capture(&control).unwrap();
    cell.register(&registry).unwrap();
    assert!(cell.register(&MemoryCaptureRegistry::new()).is_err());
    cell.write().value = 3;
    assert_eq!(**cell.captured(&after).unwrap(), 2);
    drop((before, after, cell));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_capture_controlled_read_and_write_waits_cancel_without_admission() {
    let registry = MemoryCaptureRegistry::new();
    let cell = scalar(&registry, 1);
    let control = StorageReadControl::with_limit(4096);
    let guard = cell.write();
    let (ready, entered) = mpsc::channel();
    thread::scope(|scope| {
        let mut workers = Vec::new();
        for write in [false, true] {
            let ready = ready.clone();
            let cell = &cell;
            let control = &control;
            workers.push(scope.spawn(move || {
                ON_SOURCE_WAIT.set(Some(Box::new(move || ready.send(()).unwrap())));
                let result = if write {
                    cell.write_with_control(control).map(|_| ())
                } else {
                    cell.read_with_control(control).map(|_| ())
                };
                assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
            }));
        }
        for _ in 0..2 {
            entered.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        control.cancellation().cancel();
        for worker in workers {
            worker.join().unwrap();
        }
    });
    assert_eq!(guard.value, 1);
    assert_eq!(guard.calls.load(Ordering::Relaxed), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_capture_registration_does_not_visit_sources_and_writes_preserve_each_cut_once() {
    let registry = MemoryCaptureRegistry::new();
    let first = scalar(&registry, 7);
    let small = StorageReadControl::with_limit(4096);
    let mut cut = None;
    let before = allocation_counter::measure(|| cut = Some(registry.capture(&small).unwrap()));
    assert_eq!(first.read().calls.load(Ordering::Relaxed), 0);
    let registration = small.memory().used();
    drop(cut.take());
    assert_eq!(small.memory().used(), 0);
    let cells: Vec<_> = (0..8192).map(|n| scalar(&registry, n)).collect();
    let after = allocation_counter::measure(|| cut = Some(registry.capture(&small).unwrap()));
    assert_eq!(before.count_total, after.count_total);
    assert_eq!(before.bytes_total, after.bytes_total);
    assert_eq!(small.memory().used(), registration);
    assert!(cells
        .iter()
        .all(|cell| cell.read().calls.load(Ordering::Relaxed) == 0));
    let cut = cut.unwrap();
    cells[42].write().value = 70;
    cells[42].write().value = 71;
    cells[8191].write().value = 9000;
    assert_eq!(cells[42].read().calls.load(Ordering::Relaxed), 1);
    assert_eq!(**cells[42].captured(&cut).unwrap(), 42);
    assert_eq!(**cells[8191].captured(&cut).unwrap(), 8191);
    let second_control = StorageReadControl::with_limit(4096);
    let second = registry.capture(&second_control).unwrap();
    cells[42].write().value = 72;
    assert_eq!(**cells[42].captured(&second).unwrap(), 71);
    assert_eq!(**cells[42].captured(&cut).unwrap(), 42);
    assert_eq!(cells[42].read().calls.load(Ordering::Relaxed), 2);
    drop((cut, second));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(second_control.memory().used(), 0);
    cells[42].write().value = 73;
    assert_eq!(cells[42].read().calls.load(Ordering::Relaxed), 2);
    assert!(registry.0.captures.lock().is_empty());
}

#[test]
fn memory_capture_reads_reuse_frozen_values_while_the_source_write_guard_is_live() {
    let registry = MemoryCaptureRegistry::new();
    let cell = scalar(&registry, 11);
    let control = StorageReadControl::with_limit(4096);
    let cut = registry.capture(&control).unwrap();
    let saved = cell.captured(&cut).unwrap();
    let mut writer = cell.write();
    writer.value = 22;
    // An already frozen read must not wait for the mutable source or observe its replacement.
    let retained = cell.captured(&cut).unwrap();
    assert!(Arc::ptr_eq(&saved, &retained));
    assert_eq!(**retained, 11);
    drop(writer);
    drop((cut, cell, registry));
    assert!(control.memory().used() > 0);
    assert_eq!(**saved, 11);
    drop((saved, retained));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_capture_waiting_reads_select_a_newly_archived_value_before_the_writer_finishes() {
    let registry = MemoryCaptureRegistry::new();
    let cell = Arc::new(scalar(&registry, 1));
    let control = StorageReadControl::with_limit(4096);
    let cut = registry.capture(&control).unwrap();
    let (locked, observed_lock) = mpsc::channel();
    let (preserve, may_preserve) = mpsc::channel();
    let (release, may_release) = mpsc::channel();
    let target = cell.clone();
    let writer = thread::spawn(move || {
        ON_SOURCE_LOCKED.set(Some(Box::new(move || {
            locked.send(()).unwrap();
            may_preserve.recv().unwrap();
        })));
        let mut guard = target.write();
        guard.value = 2;
        may_release.recv().unwrap();
    });
    observed_lock.recv_timeout(Duration::from_secs(5)).unwrap();
    let (waiting, observed_wait) = mpsc::channel();
    let (finished, observed_value) = mpsc::channel();
    let target = cell.clone();
    let selected = cut.clone();
    let reader = thread::spawn(move || {
        ON_SOURCE_WAIT.set(Some(Box::new(move || waiting.send(()).unwrap())));
        finished
            .send(target.captured(&selected).map(|value| **value))
            .unwrap();
    });
    observed_wait.recv_timeout(Duration::from_secs(5)).unwrap();
    preserve.send(()).unwrap();
    let result = observed_value.recv_timeout(Duration::from_secs(5));
    release.send(()).unwrap();
    writer.join().unwrap();
    reader.join().unwrap();
    assert_eq!(result.unwrap().unwrap(), 1);
    assert_eq!(cell.read().value, 2);
    drop(cut);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_capture_registration_rejects_incomplete_admission_without_retained_leases() {
    let registry = MemoryCaptureRegistry::new();
    let required = size_of::<Capture>() + OwnedMap::<u64, Weak<Capture>>::entry_bytes();
    for limit in [0, size_of::<Capture>(), required - 1, required] {
        let control = StorageReadControl::with_limit(limit);
        match registry.capture(&control) {
            Ok(capture) => {
                assert_eq!(limit, required);
                assert_eq!(control.memory().used(), required);
                drop(capture);
            }
            Err(error) => {
                assert!(limit < required);
                assert!(matches!(error, StorageBackendError::Memory(_)));
            }
        }
        assert!(registry.0.captures.lock().is_empty());
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn memory_capture_retention_failure_is_terminal_without_consuming_the_source_write() {
    let registry = MemoryCaptureRegistry::new();
    let first = scalar(&registry, 1);
    let second = registry
        .cell(Scalar {
            value: 2,
            calls: Arc::default(),
            scratch: 1 << 20,
        })
        .unwrap();
    let control = StorageReadControl::with_limit(4096);
    let cut = registry.capture(&control).unwrap();
    drop(first.captured(&cut).unwrap());
    assert!(control.memory().used() > size_of::<Capture>());
    second.write().value = 3;
    assert_eq!(second.read().value, 3);
    assert!(registry.0.captures.lock().is_empty());
    assert_eq!(control.memory().used(), size_of::<Capture>());
    control.cancellation().cancel();
    for _ in 0..2 {
        assert!(matches!(
            first.captured(&cut),
            Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
        ));
        assert!(matches!(
            cut.check(),
            Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
        ));
    }
    second.write().value = 4;
    assert_eq!(second.read().calls.load(Ordering::Relaxed), 1);
    drop(cut);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_capture_cancellation_retires_the_archive_and_preserves_other_cuts() {
    let registry = MemoryCaptureRegistry::new();
    let cell = scalar(&registry, 1);
    let first_control = StorageReadControl::with_limit(4096);
    let second_control = StorageReadControl::with_limit(4096);
    let first = registry.capture(&first_control).unwrap();
    let second = registry.capture(&second_control).unwrap();
    cell.write().value = 2;
    first_control.cancellation().cancel();
    cell.write().value = 3;
    assert!(matches!(
        first.check(),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(first_control.memory().used(), size_of::<Capture>());
    assert_eq!(**cell.captured(&second).unwrap(), 1);
    assert_eq!(registry.0.captures.lock().len(), 1);
    first_control.cancellation().reset();
    assert!(matches!(
        cell.captured(&first),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop((first, second));
    assert_eq!(first_control.memory().used(), 0);
    assert_eq!(second_control.memory().used(), 0);
}

#[test]
fn memory_capture_rejects_foreign_and_later_cells_without_invalidating_its_sources() {
    let registry = MemoryCaptureRegistry::new();
    let cell = scalar(&registry, 1);
    let control = StorageReadControl::with_limit(4096);
    let cut = registry.capture(&control).unwrap();
    let later = scalar(&registry, 2);
    let other = scalar(&MemoryCaptureRegistry::new(), 3);
    assert!(later.captured(&cut).is_err());
    assert!(other.captured(&cut).is_err());
    later.write().value = 4;
    assert_eq!(later.read().calls.load(Ordering::Relaxed), 0);
    assert_eq!(**cell.captured(&cut).unwrap(), 1);
    cut.check().unwrap();
}

fn wait_for_admission(control: &StorageReadControl) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while control.memory().used() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "capture did not enter admission"
        );
        thread::yield_now();
    }
}

#[test]
fn memory_capture_registration_waits_for_mapped_writes_and_observes_cancellation() {
    let registry = MemoryCaptureRegistry::new();
    let cell = scalar(&registry, 1);
    for cancel in [true, false] {
        let mut writer = MemoryCaptureWriteGuard::map(cell.write(), |value| &mut value.value);
        let control = StorageReadControl::with_limit(4096);
        let (send, receive) = mpsc::channel();
        let waiting = registry.clone();
        let waiting_control = control.clone();
        let worker = thread::spawn(move || send.send(waiting.capture(&waiting_control)).unwrap());
        wait_for_admission(&control);
        assert!(registry.0.captures.lock().is_empty());
        if cancel {
            control.cancellation().cancel();
            assert!(matches!(
                receive.recv_timeout(Duration::from_secs(5)).unwrap(),
                Err(StorageBackendError::Cancelled(_))
            ));
            assert_eq!(control.memory().used(), 0);
        }
        *writer = 9;
        drop(writer);
        if !cancel {
            let cut = receive
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            assert_eq!(**cell.captured(&cut).unwrap(), 9);
            drop(cut);
            assert_eq!(control.memory().used(), 0);
        }
        worker.join().unwrap();
    }
}

#[test]
fn memory_capture_nested_cell_writes_keep_registration_outside_the_complete_guard_scope() {
    let registry = MemoryCaptureRegistry::new();
    let first = scalar(&registry, 1);
    let second = scalar(&registry, 2);
    let mut first_write = first.write();
    let control = StorageReadControl::with_limit(4096);
    let waiting = registry.clone();
    let waiting_control = control.clone();
    let worker = thread::spawn(move || waiting.capture(&waiting_control));
    wait_for_admission(&control);
    let mut second_write = second.write();
    first_write.value = 3;
    second_write.value = 4;
    drop((second_write, first_write));
    let cut = worker.join().unwrap().unwrap();
    assert_eq!(**first.captured(&cut).unwrap(), 3);
    assert_eq!(**second.captured(&cut).unwrap(), 4);
}

struct Blocking {
    value: u64,
    entered: mpsc::Sender<()>,
    resume: Mutex<mpsc::Receiver<()>>,
}

impl MemoryCaptureSource for Blocking {
    type Frozen = u64;
    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<u64> {
        control.check()?;
        self.entered.send(()).unwrap();
        self.resume.lock().recv().unwrap();
        Ok(self.value)
    }
}

#[test]
fn memory_capture_last_handle_can_close_during_write_preservation_without_leaking_registration() {
    let registry = MemoryCaptureRegistry::new();
    let (entered, observe) = mpsc::channel();
    let (resume, pending) = mpsc::channel();
    let cell = Arc::new(
        registry
            .cell(Blocking {
                value: 1,
                entered,
                resume: Mutex::new(pending),
            })
            .unwrap(),
    );
    let control = StorageReadControl::with_limit(4096);
    let cut = registry.capture(&control).unwrap();
    let target = cell.clone();
    let worker = thread::spawn(move || target.write().value = 2);
    observe.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(cut);
    resume.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(cell.read().value, 2);
    assert!(registry.0.captures.lock().is_empty());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_capture_real_document_and_text_sources_keep_original_values_and_retention() {
    let registry = MemoryCaptureRegistry::new();
    let documents: Box<dyn crate::DocumentStore> = Box::new(crate::MemoryDocumentStore::new());
    let documents = registry.cell(documents).unwrap();
    documents
        .write()
        .put(
            1,
            BTreeMap::from([("body".into(), Value::Str("old shared shared".into()))]),
        )
        .unwrap();
    let mut index = crate::MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    crate::InvertedIndex::add_document(
        &mut index,
        1,
        BTreeMap::from([("body".into(), "old shared shared".into())]),
    )
    .unwrap();
    let index: Box<dyn crate::InvertedIndex> = Box::new(index);
    let index = registry.cell(index).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let cut = registry.capture(&control).unwrap();
    documents
        .write()
        .put(
            1,
            BTreeMap::from([("body".into(), Value::Str("new".into()))]),
        )
        .unwrap();
    index
        .write()
        .add_document(1, BTreeMap::from([("body".into(), "new".into())]))
        .unwrap();
    let old_documents = documents.captured(&cut).unwrap();
    let old_index = index.captured(&cut).unwrap();
    assert_eq!(
        old_documents.get(1).unwrap().unwrap()["body"],
        Value::Str("old shared shared".into())
    );
    assert_eq!(old_index.get_term_freq(1, "body", "shared").unwrap(), 2);
    assert_eq!(index.read().get_term_freq(1, "body", "shared").unwrap(), 0);
    drop((cut, documents, index, registry));
    assert!(control.memory().used() > 0);
    assert_eq!(old_index.get_term_freq(1, "body", "shared").unwrap(), 2);
    control.cancellation().cancel();
    assert!(matches!(
        old_documents.get(1),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        old_index.get_term_freq(1, "body", "shared"),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop((old_documents, old_index));
    assert_eq!(control.memory().used(), 0);
}
