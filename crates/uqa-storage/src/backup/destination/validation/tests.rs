//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct Work {
    memory: uqa_core::memory::MemoryBudget,
    calls: Arc<AtomicUsize>,
    closed: Arc<AtomicUsize>,
    remaining: usize,
    failure: &'static str,
}

impl Drop for Work {
    fn drop(&mut self) {
        assert!(
            self.memory.used() >= size_of::<Self>(),
            "work must retire before its allowance"
        );
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

impl BackupImageValidationWork for Work {
    fn advance(&mut self, maximum_steps: usize) -> BackupResult<BackupImageValidationStep> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.failure {
            "error" => return Err(BackupError::Integrity),
            "panic" => panic!("validation failed"),
            _ => (),
        }
        self.remaining = self.remaining.saturating_sub(maximum_steps);
        Ok(if self.remaining == 0 {
            BackupImageValidationStep::Complete
        } else {
            BackupImageValidationStep::Pending
        })
    }
}

#[test]
fn logical_validation_admits_before_construction_and_closes_before_releasing_its_allowance() {
    let denied = StorageReadControl::with_limit(size_of::<Work>() - 1);
    let constructed = AtomicUsize::new(0);
    assert!(OwnedBackupImageValidation::create::<Work>(&denied, || {
        constructed.fetch_add(1, Ordering::SeqCst);
        panic!("unadmitted constructor")
    })
    .is_err());
    assert_eq!(constructed.load(Ordering::SeqCst), 0);
    assert_eq!(denied.memory().used(), 0);

    let control = StorageReadControl::with_limit(size_of::<Work>());
    let calls = Arc::new(AtomicUsize::new(0));
    let closed = Arc::new(AtomicUsize::new(0));
    let mut input = OwnedBackupImageValidation::create(&control, || {
        Ok(Work {
            memory: control.memory().clone(),
            calls: calls.clone(),
            closed: closed.clone(),
            remaining: 3,
            failure: "none",
        })
    })
    .unwrap();
    for step in [
        BackupImageValidationStep::Pending,
        BackupImageValidationStep::Pending,
        BackupImageValidationStep::Complete,
    ] {
        assert_eq!(input.advance(1).unwrap(), step);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    assert_eq!(
        input.advance(0).unwrap(),
        BackupImageValidationStep::Complete
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn logical_validation_failure_cancellation_unwind_and_drop_retire_the_original_owner() {
    for failure in ["error", "panic", "cancel", "zero", "drop"] {
        let control = StorageReadControl::with_limit(size_of::<Work>());
        let calls = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicUsize::new(0));
        let mut input = OwnedBackupImageValidation::create(&control, || {
            Ok(Work {
                memory: control.memory().clone(),
                calls: calls.clone(),
                closed: closed.clone(),
                remaining: 3,
                failure,
            })
        })
        .unwrap();
        if failure == "drop" {
            drop(input);
        } else {
            if failure == "cancel" {
                control.cancellation().cancel();
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                input.advance(usize::from(failure != "zero"))
            }));
            assert!(matches!(result, Err(_) | Ok(Err(_))), "{failure}");
            assert!(matches!(input.advance(1), Err(BackupError::Finished)));
            assert_eq!(
                calls.load(Ordering::SeqCst),
                usize::from(matches!(failure, "error" | "panic"))
            );
        }
        assert_eq!(closed.load(Ordering::SeqCst), 1, "{failure}");
        assert_eq!(control.memory().used(), 0, "{failure}");
    }
}

#[test]
fn logical_validation_completed_callback_is_allocation_free_and_constructor_failure_is_retired() {
    let control = StorageReadControl::with_limit(0);
    let mut done = OwnedBackupImageValidation::completed(&control).unwrap();
    control.cancellation().cancel();
    assert_eq!(
        done.advance(0).unwrap(),
        BackupImageValidationStep::Complete
    );
    assert!(OwnedBackupImageValidation::completed(&control).is_err());
    let control = StorageReadControl::with_limit(size_of::<Work>());
    assert!(
        OwnedBackupImageValidation::create::<Work>(&control, || Err(BackupError::Integrity))
            .is_err()
    );
    assert_eq!(control.memory().used(), 0);
}
