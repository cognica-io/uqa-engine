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
use uqa_core::memory::BudgetedVec;

struct Source {
    drops: Arc<AtomicUsize>,
    bytes: BudgetedVec<u8>,
    action: u8,
}

impl Drop for Source {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl MaximumDocIdSource for Source {
    fn advance(&mut self, control: &StorageReadControl) -> StorageBackendResult<MaximumDocIdStep> {
        assert_eq!(self.bytes.len(), 17);
        match self.action {
            0 => Ok(MaximumDocIdStep::Pending),
            1 => Ok(MaximumDocIdStep::Complete(71)),
            2 => Err(StorageBackendError::Other("source failure".into())),
            3 => {
                control.cancellation().cancel();
                Ok(MaximumDocIdStep::Complete(71))
            }
            _ => panic!("source unwind"),
        }
    }
}

#[test]
fn document_maximum_retires_source_before_completion_failure_cancellation_or_unwind() {
    for action in 0..5 {
        let control = StorageReadControl::with_limit(4096);
        let drops = Arc::new(AtomicUsize::new(0));
        let mut bytes = BudgetedVec::new(control.memory());
        bytes.extend_from_slice(&[0; 17]).unwrap();
        let mut input = MaximumDocIdInput::new(
            Source {
                drops: drops.clone(),
                bytes,
                action,
            },
            &control,
        )
        .unwrap();
        assert_eq!(control.memory().used(), 17 + size_of::<Source>());
        if action == 4 {
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| input.advance())).is_err()
            );
        } else {
            let result = input.advance();
            match action {
                0 => {
                    assert_eq!(result.unwrap(), MaximumDocIdStep::Pending);
                    assert_eq!(drops.load(Ordering::SeqCst), 0);
                    control.cancellation().cancel();
                    assert!(input.advance().is_err());
                }
                1 => assert_eq!(result.unwrap(), MaximumDocIdStep::Complete(71)),
                _ => assert!(result.is_err()),
            }
        }
        assert_eq!(control.memory().used(), 0);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        control.cancellation().cancel();
        if action == 1 {
            assert_eq!(input.advance().unwrap(), MaximumDocIdStep::Complete(71));
        } else {
            assert!(input.advance().is_err());
        }
    }
}

#[test]
fn document_maximum_admission_and_abandonment_release_the_original_owner() {
    for limit in [17, 17 + size_of::<Source>()] {
        let control = StorageReadControl::with_limit(limit);
        let drops = Arc::new(AtomicUsize::new(0));
        let mut bytes = BudgetedVec::new(control.memory());
        bytes.extend_from_slice(&[0; 17]).unwrap();
        let input = MaximumDocIdInput::new(
            Source {
                drops: drops.clone(),
                bytes,
                action: 0,
            },
            &control,
        );
        assert_eq!(input.is_ok(), limit > 17);
        drop(input);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(control.memory().used(), 0);
    }
}
