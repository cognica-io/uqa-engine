//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resource hooks with fault injection.

use std::cell::RefCell;
use std::rc::Rc;

use uqa_pg_regex::RegexResources;

/// Resource hooks that account every byte and can refuse, cancel or limit at a chosen point.
#[derive(Debug, Default)]
pub struct Budget {
    /// Bytes currently reserved and not yet released.
    pub live: usize,
    /// Largest `live` value.
    pub peak: usize,
    /// Number of reservation requests, including refused ones.
    pub reservations: usize,
    /// Number of interrupt checks.
    pub interrupts: usize,
    /// Largest stack depth reported by the engine.
    pub deepest: usize,
    /// Refuse every reservation above this many live bytes.
    pub limit: Option<usize>,
    /// Refuse exactly the reservation with this zero-based index.
    pub refuse_reservation: Option<usize>,
    /// Request cancellation at the interrupt check with this zero-based index.
    pub cancel_interrupt: Option<usize>,
    /// Report the stack as too deep beyond this many bytes.
    pub stack_limit: Option<usize>,
    /// Panic at the reservation with this zero-based index.
    pub panic_reservation: Option<usize>,
    /// Panic at the interrupt check with this zero-based index.
    pub panic_interrupt: Option<usize>,
}

impl Budget {
    pub fn unlimited() -> Self {
        Self::default()
    }
}

impl RegexResources for Budget {
    fn reserve(&mut self, bytes: usize) -> bool {
        let index = self.reservations;
        self.reservations += 1;
        assert!(
            self.panic_reservation != Some(index),
            "reservation hook panic injected at {index}"
        );
        if self.refuse_reservation == Some(index)
            || self.limit.is_some_and(|limit| self.live + bytes > limit)
        {
            return false;
        }
        self.live += bytes;
        self.peak = self.peak.max(self.live);
        true
    }

    fn release(&mut self, bytes: usize) {
        self.live = self
            .live
            .checked_sub(bytes)
            .expect("the engine never releases more than it reserved");
    }

    fn cancel_requested(&mut self) -> bool {
        let index = self.interrupts;
        self.interrupts += 1;
        assert!(
            self.panic_interrupt != Some(index),
            "interrupt hook panic injected at {index}"
        );
        self.cancel_interrupt == Some(index)
    }

    fn stack_too_deep(&mut self, depth: usize) -> bool {
        self.deepest = self.deepest.max(depth);
        self.stack_limit.is_some_and(|limit| depth > limit)
    }
}

/// A [`Budget`] that the test can adjust while a compiled expression borrows the hooks.
#[derive(Clone, Default)]
pub struct SharedBudget(pub Rc<RefCell<Budget>>);

impl RegexResources for SharedBudget {
    fn reserve(&mut self, bytes: usize) -> bool {
        self.0.borrow_mut().reserve(bytes)
    }

    fn release(&mut self, bytes: usize) {
        self.0.borrow_mut().release(bytes);
    }

    fn cancel_requested(&mut self) -> bool {
        self.0.borrow_mut().cancel_requested()
    }

    fn stack_too_deep(&mut self, depth: usize) -> bool {
        self.0.borrow_mut().stack_too_deep(depth)
    }
}

pub fn code_points(text: &str) -> Vec<u32> {
    text.chars().map(u32::from).collect()
}
