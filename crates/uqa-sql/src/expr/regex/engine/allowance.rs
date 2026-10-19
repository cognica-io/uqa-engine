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
    cache: Option<&'a super::super::cache::RegexCache>,
}

impl<'a, 'c> Allowance<'a, 'c> {
    pub(super) fn new(call: &'a RegexCall<'c>) -> Self {
        Self {
            call,
            lease: call.control.empty_reservation(),
            cache: None,
        }
    }
    pub(super) fn cached(
        call: &'a RegexCall<'c>,
        cache: &'a super::super::cache::RegexCache,
    ) -> Self {
        Self {
            call,
            lease: Some(cache.budget.empty_reservation()),
            cache: Some(cache),
        }
    }

    pub(super) fn retain(&mut self) -> super::super::cache::RetainedAllowance {
        super::super::cache::RetainedAllowance(self.lease.take())
    }
}

impl RegexResources for Allowance<'_, '_> {
    fn reserve(&mut self, bytes: usize) -> bool {
        let Some(lease) = self.lease.as_mut() else {
            return true;
        };
        loop {
            match lease.grow(bytes) {
                Ok(()) => return true,
                Err(error) => {
                    if self
                        .cache
                        .is_some_and(super::super::cache::RegexCache::evict)
                    {
                        continue;
                    }
                    self.call.refuse(error);
                    return false;
                }
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
