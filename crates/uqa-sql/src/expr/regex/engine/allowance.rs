//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The engine's resource hooks over one call's allowance.

use super::RegexCall;
use uqa_core::memory::MemoryReservation;
use uqa_pg_regex::RegexResources;

/// Engine hooks for one call: every engine allocation grows one lease of the call's allowance and every release splits its bytes off again, so the lease is empty once the compiled expression is dropped.
pub(in crate::expr::regex) struct Allowance<'a, 'c> {
    call: &'a RegexCall<'c>,
    lease: Option<MemoryReservation>,
}

impl<'a, 'c> Allowance<'a, 'c> {
    pub(super) fn new(call: &'a RegexCall<'c>) -> Self {
        Self {
            call,
            lease: call.control.empty_reservation(),
        }
    }
}

impl RegexResources for Allowance<'_, '_> {
    fn reserve(&mut self, bytes: usize) -> bool {
        let Some(lease) = self.lease.as_mut() else {
            return true;
        };
        match lease.grow(bytes) {
            Ok(()) => true,
            Err(error) => {
                self.call.refuse(error);
                false
            }
        }
    }

    fn release(&mut self, bytes: usize) {
        if let Some(lease) = self.lease.as_mut() {
            drop(lease.split(bytes));
        }
    }

    fn cancel_requested(&mut self) -> bool {
        self.call.cancel_requested()
    }

    /// UQA has no `max_stack_depth` setting, so the engine's own limit, `PostgreSQL`'s default of 2 MB, is the only one.
    fn stack_too_deep(&mut self, _depth: usize) -> bool {
        false
    }
}
