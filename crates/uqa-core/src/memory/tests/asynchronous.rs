//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{
    future::{poll_fn, Future},
    sync::{atomic::AtomicUsize, Arc},
    task::{Context, Poll, Waker},
};

struct Input {
    memory: MemoryBudget,
    observed: Arc<AtomicUsize>,
}

impl Drop for Input {
    fn drop(&mut self) {
        self.observed.store(self.memory.used(), Ordering::SeqCst);
    }
}

#[test]
fn suspended_owned_mapping_keeps_input_admission_until_transformation_retirement() {
    for mode in [
        "complete",
        "error",
        "unwind",
        "construct",
        "pending",
        "unpolled",
    ] {
        let size = size_of::<Input>();
        let memory = MemoryBudget::new(size);
        let observed = Arc::new(AtomicUsize::new(0));
        let owner = Budgeted::new(
            Box::new(Input {
                memory: memory.clone(),
                observed: observed.clone(),
            }),
            memory.reserve(size).unwrap(),
        );
        let mut future = Box::pin(owner.try_map_async(|input| {
            assert_ne!(mode, "construct", "transformation constructor unwound");
            async move {
                let mut first = true;
                poll_fn(|_| {
                    if std::mem::take(&mut first) {
                        Poll::Pending
                    } else {
                        Poll::Ready(())
                    }
                })
                .await;
                match mode {
                    "error" => Err(()),
                    "unwind" => panic!("suspended transformation unwound"),
                    _ => Ok(input),
                }
            }
        }));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut context = Context::from_waker(Waker::noop());
            if mode == "unpolled" {
                return None;
            }
            assert!(future.as_mut().poll(&mut context).is_pending());
            assert_eq!(memory.used(), size);
            assert_eq!(observed.load(Ordering::SeqCst), 0);
            if mode == "pending" {
                return None;
            }
            let Poll::Ready(value) = future.as_mut().poll(&mut context) else {
                panic!("second poll must complete");
            };
            assert_eq!(value.is_err(), mode == "error");
            Some(value)
        }));
        assert_eq!(result.is_err(), matches!(mode, "construct" | "unwind"));
        drop((future, result));
        assert_eq!(observed.load(Ordering::SeqCst), size, "{mode}");
        assert_eq!(memory.used(), 0, "{mode}");
    }
}
