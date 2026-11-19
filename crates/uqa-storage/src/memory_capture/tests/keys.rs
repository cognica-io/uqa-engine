//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn keys_read_unchanged_sources_and_preserve_the_selected_incarnation() {
    let registry = MemoryCaptureRegistry::new();
    let cell = scalar(&registry, 7);
    let key = cell.key().unwrap();
    let control = StorageReadControl::with_limit(4096);
    let cut = registry.capture(&control).unwrap();
    assert!(key.retained(&cut).unwrap().is_none());
    let saved = key.captured(&cut).unwrap();
    assert_eq!(**saved, 7);
    assert!(Arc::ptr_eq(&saved, &cell.captured(&cut).unwrap()));
    let mut writer = cell.write();
    writer.value = 8;
    assert!(Arc::ptr_eq(&saved, &key.captured(&cut).unwrap()));
    drop(writer);
    let later = registry.capture(&control).unwrap();
    let replacement = scalar(&registry, 9);
    let replacement_key = replacement.key().unwrap();
    assert!(!key.same_source(&replacement_key));
    assert!(replacement_key.captured(&later).is_err());
    let foreign = MemoryCaptureRegistry::new().capture(&control).unwrap();
    assert!(key.captured(&foreign).is_err());
    drop((cell, replacement, registry));
    assert_eq!(**key.captured(&later).unwrap(), 8);
    assert_eq!(**key.captured(&cut).unwrap(), 7);
    assert!(replacement_key.captured(&later).is_err());
    assert!(key.captured(&foreign).is_err());
    drop((saved, cut, later, foreign));
    assert_eq!(control.memory().used(), 0);
}

struct Bytes(Arc<Vec<u8>>);

impl MemoryCaptureSource for Bytes {
    type Frozen = CapturedMemoryValue<Vec<u8>>;

    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        control.check()?;
        let memory = control.memory().reserve(self.0.len())?;
        CapturedMemoryValue::new(Budgeted::new(self.0.as_ref().clone(), memory), control)
    }
}

#[test]
fn keys_do_not_retain_later_source_payloads_and_frozen_values_keep_their_controls() {
    let registry = MemoryCaptureRegistry::new();
    let initial = Arc::new(vec![3; 128]);
    let original = Arc::downgrade(&initial);
    let cell = registry.cell(Bytes(initial)).unwrap();
    let key = cell.key().unwrap();
    let copied = allocation_counter::measure(|| drop(key.clone()));
    assert_eq!(copied.bytes_total, 0);
    let control = StorageReadControl::with_limit(4096);
    let cut = registry.capture(&control).unwrap();
    let saved = key.captured(&cut).unwrap();
    let larger = Arc::new(vec![5; 512 * 1024]);
    let future = Arc::downgrade(&larger);
    cell.write().0 = larger;
    assert!(original.upgrade().is_none());
    drop((cell, registry));
    assert!(future.upgrade().is_none());
    assert!(Arc::ptr_eq(&saved, &key.captured(&cut).unwrap()));
    assert_eq!(saved.get().unwrap(), &[3; 128]);
    drop(cut);
    assert!(control.memory().used() > 0);
    control.cancellation().cancel();
    assert!(saved.get().is_err());
    drop(saved);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn key_reads_keep_independent_admission_and_cancel_while_waiting_for_a_writer() {
    let registry = MemoryCaptureRegistry::new();
    let cell = scalar(&registry, 7);
    let key = cell.key().unwrap();
    let small = StorageReadControl::with_limit(1024);
    let good = StorageReadControl::with_limit(8192);
    let limited = registry.capture(&small).unwrap();
    let saved = registry.capture(&good).unwrap();
    cell.value().write().scratch = 2048;
    assert!(key.captured(&limited).is_err());
    assert_eq!(**key.captured(&saved).unwrap(), 7);
    cell.write().value = 8;
    assert_eq!(**key.captured(&saved).unwrap(), 7);
    let cancellation = StorageReadControl::with_limit(8192);
    let cancelled = registry.capture(&cancellation).unwrap();
    // Pause before mutation admission so the cut has no archived value yet.
    let held = cell.value().write();
    let (waiting, observed_wait) = mpsc::channel();
    thread::scope(|scope| {
        let reader = scope.spawn(|| {
            ON_SOURCE_WAIT.set(Some(Box::new(move || waiting.send(()).unwrap())));
            key.captured(&cancelled)
        });
        observed_wait.recv_timeout(Duration::from_secs(5)).unwrap();
        cancellation.cancellation().cancel();
        assert!(matches!(
            reader.join().unwrap(),
            Err(StorageBackendError::Cancelled(_))
        ));
    });
    drop(held);
    drop((cell, registry));
    assert!(key.captured(&limited).is_err());
    assert!(key.captured(&cancelled).is_err());
    assert_eq!(**key.captured(&saved).unwrap(), 7);
    drop((limited, saved, cancelled));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
    assert_eq!(cancellation.memory().used(), 0);
}

#[test]
fn key_reads_racing_source_retirement_observe_the_completed_preservation() {
    let registry = MemoryCaptureRegistry::new();
    let (entered, observe) = mpsc::channel();
    let (resume, pending) = mpsc::channel();
    let cell = registry
        .cell(Blocking {
            value: 11,
            entered,
            resume: Mutex::new(pending),
        })
        .unwrap();
    let key = cell.key().unwrap();
    let control = StorageReadControl::with_limit(4096);
    let cut = registry.capture(&control).unwrap();
    let retiring = thread::spawn(move || drop(cell));
    observe.recv_timeout(Duration::from_secs(5)).unwrap();
    // The owner is still live while Drop preserves the cut. A key can select it without retaining the enclosing table or Engine.
    let live = key
        .source
        .upgrade()
        .expect("retirement keeps the source live");
    let (ready, started) = mpsc::channel();
    let selected = cut.clone();
    let reader = thread::spawn(move || {
        ready.send(()).unwrap();
        live.captured(&selected)
    });
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    resume.send(()).unwrap();
    retiring.join().unwrap();
    let frozen = reader.join().unwrap().unwrap();
    assert_eq!(**frozen, 11);
    assert!(key.source.upgrade().is_none());
    assert!(Arc::ptr_eq(&frozen, &key.captured(&cut).unwrap()));
    drop((cut, frozen, registry));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn a_source_that_unwinds_during_retirement_cannot_leave_a_usable_incomplete_cut() {
    struct Unwinding;
    impl MemoryCaptureSource for Unwinding {
        type Frozen = ();

        fn freeze(&self, _: &StorageReadControl) -> StorageBackendResult<()> {
            panic!("injected source retirement failure");
        }
    }
    let registry = MemoryCaptureRegistry::new();
    let cell = registry.cell(Unwinding).unwrap();
    let key = cell.key().unwrap();
    let other = scalar(&registry, 7);
    let control = StorageReadControl::with_limit(4096);
    let cut = registry.capture(&control).unwrap();
    assert_eq!(**other.captured(&cut).unwrap(), 7);
    let retained = control.memory().used();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(cell))).is_err());
    assert!(key.source.upgrade().is_none());
    let error = key.captured(&cut).expect_err("an incomplete cut fails");
    assert!(error
        .to_string()
        .contains("retired memory source has no selected value"));
    assert!(control.memory().used() < retained);
    control.cancellation().cancel();
    assert_eq!(
        other.captured(&cut).unwrap_err().to_string(),
        error.to_string()
    );
    assert_eq!(cut.check().unwrap_err().to_string(), error.to_string());
    drop((cut, other, registry));
    assert_eq!(control.memory().used(), 0);
}
