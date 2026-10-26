//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dynamic execution bindings are captured explicitly when work changes threads or requests.

use super::{BlockClass, BlockOperation, IOEvent, IOObservation};
use std::sync::atomic::Ordering;
use std::{cell::RefCell, marker::PhantomData, rc::Rc, time::Instant};

thread_local! {
    static CURRENT: RefCell<IOContext> = const { RefCell::new(IOContext(None)) };
}

#[derive(Clone, Default)]
pub struct IOContext(Option<IOObservation>);

impl IOContext {
    pub fn new(observation: Option<IOObservation>) -> Self {
        Self(observation)
    }

    pub fn current() -> Self {
        CURRENT.with(|current| current.borrow().clone())
    }

    /// Keep an active child operation only when it belongs to this invocation owner.
    pub fn for_observation(observation: Option<&IOObservation>) -> Self {
        let Some(observation) = observation else {
            return Self::default();
        };
        CURRENT.with(|current| {
            Self(Some(
                current
                    .borrow()
                    .0
                    .as_ref()
                    .filter(|current| current.is_within(observation))
                    .unwrap_or(observation)
                    .clone(),
            ))
        })
    }

    pub fn observation(&self) -> Option<&IOObservation> {
        self.0.as_ref()
    }

    #[must_use]
    pub fn enter(&self) -> IOScope {
        let previous = CURRENT.with(|current| current.replace(self.clone()));
        IOScope {
            previous,
            _thread: PhantomData,
        }
    }

    pub fn record(event: IOEvent) {
        CURRENT.with(|current| {
            if let Some(observation) = &current.borrow().0 {
                observation.record(event);
            }
        });
    }

    /// Capture attribution before starting physical work. No clock is read when timing is disabled, and failed I/O is not reported as a completed block operation.
    pub fn block_operation(
        class: BlockClass,
        operation: BlockOperation,
        count: u64,
    ) -> Option<ObservedBlockOperation> {
        CURRENT.with(|current| {
            let observation = current.borrow().0.clone()?;
            let requested = observation.0.requested;
            if !requested.buffers {
                return None;
            }
            let started = (matches!(operation, BlockOperation::Read | BlockOperation::Write)
                && observation.0.timing.load(Ordering::Acquire))
            .then(Instant::now);
            Some(ObservedBlockOperation {
                observation,
                class,
                operation,
                count,
                started,
            })
        })
    }
}

pub struct IOScope {
    previous: IOContext,
    _thread: PhantomData<Rc<()>>,
}

impl IOScope {
    /// A suspended worker rebinds on every request, including explicit absence.
    pub fn replace(&self, context: &IOContext) {
        CURRENT.with(|current| current.borrow_mut().clone_from(context));
    }
}

impl Drop for IOScope {
    fn drop(&mut self) {
        CURRENT.with(|current| current.borrow_mut().clone_from(&self.previous));
    }
}

#[must_use]
pub struct ObservedBlockOperation {
    observation: IOObservation,
    class: BlockClass,
    operation: BlockOperation,
    count: u64,
    pub(super) started: Option<Instant>,
}

impl ObservedBlockOperation {
    pub fn complete(self) {
        self.observation.record(IOEvent::Blocks {
            class: self.class,
            operation: self.operation,
            count: self.count,
            elapsed: self
                .started
                .map_or(std::time::Duration::ZERO, |started| started.elapsed()),
        });
    }
}
