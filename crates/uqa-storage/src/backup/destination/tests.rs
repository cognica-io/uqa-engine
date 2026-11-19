//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Input {
    memory: uqa_core::memory::MemoryBudget,
    cancellation: uqa_core::CancellationToken,
    closed: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
    validations: Arc<AtomicUsize>,
    failure: &'static str,
}

impl Drop for Input {
    fn drop(&mut self) {
        if self.memory.used() >= size_of::<Self>() {
            self.closed.fetch_add(1, Ordering::SeqCst);
        } else {
            self.closed.store(usize::MAX, Ordering::SeqCst);
        }
    }
}

impl PersistentBackupImport for Input {
    fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<BackupImageImportStep> {
        assert_eq!(maximum_object_bytes, 17);
        self.reads.fetch_add(1, Ordering::SeqCst);
        match self.failure {
            "read_error" => Err(BackupError::Integrity),
            "read_panic" => panic!("input failed"),
            "read_cancel" => {
                self.cancellation.cancel();
                Ok(BackupImageImportStep::Imported)
            }
            _ => Ok(BackupImageImportStep::Imported),
        }
    }

    fn validate(&mut self, _: &mut dyn BackupImageValidator) -> BackupResult<()> {
        self.validations.fetch_add(1, Ordering::SeqCst);
        match self.failure {
            "validation_error" => Err(BackupError::Integrity),
            "validation_panic" => panic!("validation failed"),
            "validation_cancel" => {
                self.cancellation.cancel();
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

struct Validator;
impl BackupImageValidator for Validator {
    fn validate(
        &mut self,
        _: Arc<dyn PersistentStorageProvider>,
        _: &StorageReadControl,
    ) -> BackupResult<()> {
        unreachable!("the fixture has no physical provider")
    }
}

#[test]
fn persistent_import_admits_original_header_before_constructor_and_retires_after_validation() {
    let denied = StorageReadControl::with_limit(size_of::<Input>() - 1);
    let constructed = AtomicUsize::new(0);
    assert!(OwnedPersistentBackupImport::create::<Input>(&denied, || {
        constructed.fetch_add(1, Ordering::SeqCst);
        panic!("unadmitted constructor");
    })
    .is_err());
    assert_eq!(constructed.load(Ordering::SeqCst), 0);
    assert_eq!(denied.memory().used(), 0);

    let control = StorageReadControl::with_limit(size_of::<Input>());
    let closed = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let validations = Arc::new(AtomicUsize::new(0));
    let mut input = OwnedPersistentBackupImport::create(&control, || {
        Ok(Input {
            memory: control.memory().clone(),
            cancellation: control.cancellation().clone(),
            closed: closed.clone(),
            reads: reads.clone(),
            validations: validations.clone(),
            failure: "none",
        })
    })
    .unwrap();
    assert_eq!(control.memory().used(), size_of::<Input>());
    assert!(input.validate(&mut Validator).is_err());
    assert_eq!(closed.load(Ordering::SeqCst), 0);
    assert_eq!(input.advance(17).unwrap(), BackupImageImportStep::Imported);
    assert_eq!(input.advance(0).unwrap(), BackupImageImportStep::Imported);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    input.validate(&mut Validator).unwrap();
    assert_eq!(control.memory().used(), 0);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    control.cancellation().cancel();
    input.validate(&mut Validator).unwrap();
    assert_eq!(input.advance(0).unwrap(), BackupImageImportStep::Imported);
    assert_eq!(validations.load(Ordering::SeqCst), 1);
}

#[test]
fn persistent_import_failures_and_unwinds_retire_input_without_reentry() {
    for failure in [
        "zero_window",
        "read_error",
        "read_panic",
        "read_cancel",
        "validation_error",
        "validation_panic",
        "validation_cancel",
    ] {
        let control = StorageReadControl::with_limit(size_of::<Input>());
        let closed = Arc::new(AtomicUsize::new(0));
        let reads = Arc::new(AtomicUsize::new(0));
        let validations = Arc::new(AtomicUsize::new(0));
        let mut input = OwnedPersistentBackupImport::create(&control, || {
            Ok(Input {
                memory: control.memory().clone(),
                cancellation: control.cancellation().clone(),
                closed: closed.clone(),
                reads: reads.clone(),
                validations: validations.clone(),
                failure,
            })
        })
        .unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            input.advance(if failure == "zero_window" { 0 } else { 17 })?;
            input.validate(&mut Validator)
        }));
        assert!(matches!(result, Err(_) | Ok(Err(_))), "{failure}");
        assert_eq!(control.memory().used(), 0, "{failure}");
        assert_eq!(closed.load(Ordering::SeqCst), 1, "{failure}");
        assert!(matches!(input.advance(17), Err(BackupError::Finished)));
        assert!(matches!(
            input.validate(&mut Validator),
            Err(BackupError::Finished)
        ));
        assert_eq!(
            reads.load(Ordering::SeqCst),
            usize::from(failure != "zero_window")
        );
        assert_eq!(
            validations.load(Ordering::SeqCst),
            usize::from(failure.starts_with("validation"))
        );
    }
}

struct SteppedInput {
    original: Input,
    remaining: usize,
}

impl PersistentBackupImport for SteppedInput {
    fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<BackupImageImportStep> {
        self.original.advance(maximum_object_bytes)
    }

    fn validate(&mut self, _: &mut dyn BackupImageValidator) -> BackupResult<()> {
        panic!("bounded validation must not call the synchronous adapter")
    }

    fn advance_validation(
        &mut self,
        _: &mut dyn BackupImageValidator,
        maximum_steps: usize,
    ) -> BackupResult<BackupImageValidationStep> {
        self.original.validations.fetch_add(1, Ordering::SeqCst);
        self.remaining = self.remaining.saturating_sub(maximum_steps);
        Ok(if self.remaining == 0 {
            BackupImageValidationStep::Complete
        } else {
            BackupImageValidationStep::Pending
        })
    }
}

#[test]
fn persistent_import_retains_validation_between_calls_and_never_reopens_physical_input() {
    let control = StorageReadControl::with_limit(size_of::<SteppedInput>());
    let closed = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let validations = Arc::new(AtomicUsize::new(0));
    let mut input = OwnedPersistentBackupImport::create(&control, || {
        Ok(SteppedInput {
            original: Input {
                memory: control.memory().clone(),
                cancellation: control.cancellation().clone(),
                closed: closed.clone(),
                reads: reads.clone(),
                validations: validations.clone(),
                failure: "none",
            },
            remaining: 3,
        })
    })
    .unwrap();
    assert!(input.advance_validation(&mut Validator, 1).is_err());
    assert_eq!(input.advance(17).unwrap(), BackupImageImportStep::Imported);
    for _ in 0..2 {
        assert_eq!(
            input.advance_validation(&mut Validator, 1).unwrap(),
            BackupImageValidationStep::Pending
        );
        assert_eq!(input.advance(0).unwrap(), BackupImageImportStep::Imported);
        assert_eq!(closed.load(Ordering::SeqCst), 0);
    }
    assert_eq!(
        input.advance_validation(&mut Validator, 1).unwrap(),
        BackupImageValidationStep::Complete
    );
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(validations.load(Ordering::SeqCst), 3);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    assert_eq!(control.memory().used(), 0);
    input.validate(&mut Validator).unwrap();
    assert_eq!(validations.load(Ordering::SeqCst), 3);
}

#[test]
fn persistent_import_cancellation_retires_before_invoking_physical_or_validation_work() {
    for imported in [false, true] {
        let control = StorageReadControl::with_limit(size_of::<Input>());
        let closed = Arc::new(AtomicUsize::new(0));
        let reads = Arc::new(AtomicUsize::new(0));
        let validations = Arc::new(AtomicUsize::new(0));
        let mut input = OwnedPersistentBackupImport::create(&control, || {
            Ok(Input {
                memory: control.memory().clone(),
                cancellation: control.cancellation().clone(),
                closed: closed.clone(),
                reads: reads.clone(),
                validations: validations.clone(),
                failure: "none",
            })
        })
        .unwrap();
        if imported {
            assert_eq!(input.advance(17).unwrap(), BackupImageImportStep::Imported);
        }
        control.cancellation().cancel();
        let result = if imported {
            input.validate(&mut Validator)
        } else {
            input.advance(17).map(|_| ())
        };
        assert!(matches!(result, Err(BackupError::Cancelled(_))));
        assert_eq!(reads.load(Ordering::SeqCst), usize::from(imported));
        assert_eq!(validations.load(Ordering::SeqCst), 0);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert_eq!(control.memory().used(), 0);
        assert!(matches!(input.advance(17), Err(BackupError::Finished)));
        assert!(matches!(
            input.validate(&mut Validator),
            Err(BackupError::Finished)
        ));
    }
}
