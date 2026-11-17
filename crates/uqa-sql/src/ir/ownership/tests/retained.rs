//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ir::{ConstantValueSource, RetainedConstantValue, RetainedScalarLiteral};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Barrier,
};
use uqa_core::{memory::Produced, OwnedValue};

fn value(depth: usize, width: usize) -> Value {
    let mut value = if width == 0 {
        Value::Str("payload".into())
    } else {
        Value::Record(
            (0..width)
                .map(|i| (format!("field_{i}"), Value::Str("payload".into())))
                .collect::<Vec<_>>()
                .into(),
        )
    };
    for _ in 0..depth {
        value = Value::List(vec![value]);
    }
    value
}

fn literal(control: &ProductionControl<'_>, depth: usize, width: usize) -> RetainedScalarLiteral {
    let value = RetainedConstantValue::from_produced(
        control.retain_external_value(value(depth, width)).unwrap(),
        control,
    )
    .unwrap();
    RetainedScalarLiteral::with_control(
        value,
        Some(ty(depth).clone_with_control(control).unwrap()),
        Some(control.copy_text("public.original").unwrap()),
        Some(951),
        control,
    )
    .unwrap()
}

#[test]
fn retained_native_values_share_iterative_reads_and_failed_publication_cleanup() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for mode in ["read", "admission", "cancel", "metadata"] {
                let memory = MemoryBudget::new(64 << 20);
                let token = CancellationToken::new();
                let control = ProductionControl::new(&memory, &token, &token);
                let source = control.retain_external_value(value(8192, 0)).unwrap();
                if mode == "admission" || mode == "cancel" {
                    let pressure = memory.reserve(memory.available()).unwrap();
                    if mode == "cancel" {
                        token.cancel();
                    }
                    let error = RetainedConstantValue::from_produced(source, &control).unwrap_err();
                    assert_eq!(
                        error.sqlstate(),
                        Some(if mode == "cancel" { "57014" } else { "53200" })
                    );
                    assert_eq!(memory.used(), pressure.bytes());
                    drop(pressure);
                } else {
                    let source = RetainedConstantValue::from_produced(source, &control).unwrap();
                    if mode == "metadata" {
                        let ty = ty(8192).clone_with_control(&control).unwrap();
                        let pressure = memory.reserve(memory.available()).unwrap();
                        assert_eq!(
                            RetainedScalarLiteral::with_control(
                                source,
                                Some(ty),
                                None,
                                None,
                                &control
                            )
                            .unwrap_err()
                            .sqlstate(),
                            Some("53200")
                        );
                        assert_eq!(memory.used(), pressure.bytes());
                        drop(pressure);
                    } else {
                        let held = memory.used();
                        let pointer = std::ptr::from_ref(source.resident_value().unwrap());
                        let independent = source.clone();
                        assert!(source.shares_value_with(&independent));
                        drop(source);
                        assert_eq!(memory.used(), held);
                        assert_eq!(
                            std::ptr::from_ref(independent.resident_value().unwrap()),
                            pointer
                        );
                        let copied = OwnedValue::new(independent.read().unwrap());
                        let mut child = &*copied;
                        for _ in 0..8192 {
                            let Value::List(values) = child else {
                                panic!("native list")
                            };
                            assert_eq!(values.len(), 1);
                            child = &values[0];
                        }
                        assert_eq!(child, &Value::Str("payload".into()));
                        drop((copied, independent));
                    }
                }
                assert_eq!(memory.used(), 0);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn retained_literal_final_owners_bound_value_and_type_retirement_without_copying() {
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let sibling = memory.reserve(37).unwrap();
    let source = literal(&control, 4096, 4096);
    let independent = source.clone();
    let pointer = std::ptr::from_ref(independent.value.resident_value().unwrap());
    let retained = memory.used();
    let mut first = Retirement::new(ScalarExpr::RetainedLiteral(source));
    let mut steps = 0;
    while !first.advance() {
        steps += 1;
    }
    assert!(steps < 8);
    assert_eq!(memory.used(), retained);
    assert_eq!(
        std::ptr::from_ref(independent.value.resident_value().unwrap()),
        pointer
    );
    let mut last = Retirement::new(ScalarExpr::RetainedLiteral(independent));
    let mut freed_bytes = 0;
    let mut steps = 0;
    loop {
        let mut complete = false;
        let released = allocation_counter::measure(|| complete = last.advance());
        assert_eq!(released.count_total, 0);
        assert!(-released.count_current <= 8, "{released:?}");
        freed_bytes += usize::try_from(-released.bytes_current).unwrap();
        assert!(
            retained - memory.used() <= freed_bytes,
            "payload reservation released before its allocation"
        );
        steps += 1;
        assert!(steps < 100_000);
        if complete {
            break;
        }
    }
    assert!(steps > 8192);
    assert_eq!(memory.used(), sibling.bytes());
}

#[test]
fn retained_literal_retirement_keeps_original_leases_at_every_pending_prefix() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let sibling = native.reserve(37).unwrap();
    let mut prefixes = 0;
    for prefix in 0..512 {
        let mut complete = false;
        for cancellation in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&native, &original, &invoking);
            let source = literal(&control, 7, 3);
            let value = control
                .finish(
                    ScalarExpr::RetainedLiteral(source),
                    control.empty_reservation(),
                )
                .unwrap();
            let mut input = owned_input(value, &native, &work, &original, &invoking);
            let pressure = native.reserve(native.available()).unwrap();
            let retained = native.used();
            let mut freed_bytes = 0;
            for _ in 0..prefix {
                let mut result = None;
                let released =
                    allocation_counter::measure(|| result = advance(&mut input).unwrap());
                assert_eq!(released.count_total, 0);
                freed_bytes += usize::try_from(-released.bytes_current).unwrap();
                assert!(retained - native.used() <= freed_bytes);
                if result.is_some() {
                    complete = true;
                    break;
                }
            }
            if !complete && cancellation != 0 {
                if cancellation == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut input).unwrap_err().sqlstate(), Some("57014"));
            }
            assert_eq!(allocation_counter::measure(|| drop(input)).count_total, 0);
            drop(pressure);
            assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
        }
        prefixes += 1;
        if complete {
            break;
        }
    }
    assert!(prefixes > 32 && prefixes < 512);
}

#[test]
fn retained_literal_concurrent_final_references_transfer_one_native_payload() {
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let value = RetainedConstantValue::from_produced(
        control.retain_external_value(value(8192, 0)).unwrap(),
        &control,
    )
    .unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let literal =
            RetainedScalarLiteral::with_control(value.clone(), None, None, None, &control).unwrap();
        let barrier = barrier.clone();
        threads.push(
            std::thread::Builder::new()
                .stack_size(256 * 1024)
                .spawn(move || {
                    let mut state = Retirement::new(ScalarExpr::RetainedLiteral(literal));
                    barrier.wait();
                    let mut steps = 0;
                    loop {
                        let mut complete = false;
                        let released = allocation_counter::measure(|| complete = state.advance());
                        assert_eq!(released.count_total, 0);
                        assert!(-released.count_current <= 8, "{released:?}");
                        steps += 1;
                        assert!(steps < 65_536);
                        if complete {
                            return steps;
                        }
                    }
                })
                .unwrap(),
        );
    }
    drop(value);
    barrier.wait();
    let steps: usize = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .sum();
    assert!(steps > 8192);
    assert_eq!(memory.used(), 0);
}

#[derive(Debug)]
struct CancelAfterRead {
    token: CancellationToken,
    reads: AtomicUsize,
}

impl ConstantValueSource for CancelAfterRead {
    fn read(&self, _: u64) -> Result<Value, SQLError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let value = value(8192, 0);
        self.token.cancel();
        Ok(value)
    }

    fn read_with_control(
        &self,
        _: u64,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let value = control.retain_external_value(value(8192, 0))?;
        self.token.cancel();
        Ok(value)
    }
}

#[test]
fn retained_value_reads_retire_completed_external_payloads_after_late_cancellation() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for mode in ["ordinary", "controlled", "capture"] {
                let memory = MemoryBudget::new(64 << 20);
                let token = CancellationToken::new();
                let control = ProductionControl::new(&memory, &token, &token);
                let source = Arc::new(CancelAfterRead {
                    token: token.clone(),
                    reads: AtomicUsize::new(0),
                });
                let value = RetainedConstantValue::new(source.clone(), 0);
                let scope = crate::expr::StatementCancellationScope::enter(token.clone());
                let error = match mode {
                    "ordinary" => value.read().unwrap_err(),
                    "controlled" => value.read_with_control(&control).unwrap_err(),
                    "capture" => value.capture_with_control(&control).unwrap_err(),
                    _ => unreachable!(),
                };
                assert_eq!(error.sqlstate(), Some("57014"));
                assert_eq!(source.reads.load(Ordering::Relaxed), 1);
                assert_eq!(memory.used(), 0);
                drop(scope);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[derive(Debug, Default)]
struct CapturePressure {
    pressure: std::sync::Mutex<Option<MemoryReservation>>,
    reads: AtomicUsize,
}

impl ConstantValueSource for CapturePressure {
    fn read(&self, _: u64) -> Result<Value, SQLError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(value(8192, 0))
    }

    fn read_with_control(
        &self,
        _: u64,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let value = control.retain_external_value(value(8192, 0))?;
        let budget = control.budget().expect("controlled capture");
        *self.pressure.lock().unwrap() = Some(budget.reserve(budget.available())?);
        Ok(value)
    }
}

#[test]
fn retained_capture_keeps_completed_values_owned_through_later_metadata_failures() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for mode in ["type", "declaration", "header"] {
                let source = Arc::new(CapturePressure::default());
                let literal = RetainedScalarLiteral::new(
                    RetainedConstantValue::new(source.clone(), 0),
                    (mode == "type").then(|| ColumnType::Array(Box::new(ColumnType::Text))),
                    (mode == "declaration").then(|| "public.original".into()),
                    None,
                );
                let memory = MemoryBudget::new(64 << 20);
                let token = CancellationToken::new();
                let control = ProductionControl::new(&memory, &token, &token);
                let error = literal.capture_with_footprint(&control).unwrap_err();
                assert_eq!(error.sqlstate(), Some("53200"));
                assert_eq!(source.reads.load(Ordering::Relaxed), 1);
                let pressure = source.pressure.lock().unwrap().take().unwrap();
                assert_eq!(memory.used(), pressure.bytes());
                drop(pressure);
                assert_eq!(memory.used(), 0);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[derive(Debug)]
struct ExternalResource {
    reads: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}

impl ConstantValueSource for ExternalResource {
    fn read(&self, index: u64) -> Result<Value, SQLError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        assert_eq!(index, 23);
        Ok(Value::Int(7))
    }
}

impl Drop for ExternalResource {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn retained_literal_retirement_preserves_opaque_external_resource_ownership() {
    let reads = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(ExternalResource {
        reads: reads.clone(),
        drops: drops.clone(),
    });
    let value = RetainedConstantValue::new(source.clone(), 23);
    let independent = value.clone();
    let literal = RetainedScalarLiteral::new(value, None, None, None);
    let mut retirement = Retirement::new(ScalarExpr::RetainedLiteral(literal));
    assert_eq!(
        allocation_counter::measure(|| while !retirement.advance() {}).count_total,
        0
    );
    assert_eq!(reads.load(Ordering::Relaxed), 0);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert_eq!(independent.read().unwrap(), Value::Int(7));
    assert_eq!(reads.load(Ordering::Relaxed), 1);
    drop(source);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    drop(independent);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn retained_external_storage_admission_keeps_native_cleanup_before_handoff() {
    #[derive(Debug)]
    struct Storage;
    impl crate::ir::ConstantValueStorage for Storage {
        fn retain(&self, _: Value) -> Result<RetainedConstantValue, SQLError> {
            panic!("cancelled input cannot reach external publication")
        }
    }
    use crate::ir::ConstantValueStorage;
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for original_cancelled in [true, false] {
                let memory = MemoryBudget::new(64 << 20);
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let control = ProductionControl::new(&memory, &original, &invoking);
                let value = control.retain_external_value(value(8192, 0)).unwrap();
                if original_cancelled {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(
                    Storage
                        .retain_produced(value, &control)
                        .unwrap_err()
                        .sqlstate(),
                    Some("57014")
                );
                assert_eq!(memory.used(), 0);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
