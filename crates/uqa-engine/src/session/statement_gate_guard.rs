//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A hold of the session's statement gate that marks where the session's busy and idle periods begin.

use super::Engine;

/// A hold of the statement gate. Taking the outermost hold begins a busy period and releasing it begins an idle period, whose session timeouts the engine then schedules.
pub(crate) struct StatementGateGuard<'a> {
    engine: &'a Engine,
    _guard: Option<parking_lot::ReentrantMutexGuard<'a, ()>>,
    counted: bool,
}

impl Drop for StatementGateGuard<'_> {
    fn drop(&mut self) {
        if self.counted && self.engine.runtime.terminations.leave_gate() {
            self.engine.session_became_idle();
        }
    }
}

impl Engine {
    /// Hold the session's statement gate for an operation.
    pub(crate) fn lock_statement_gate(&self) -> StatementGateGuard<'_> {
        let guard = self.runtime.statement_gate.lock();
        let counted = guard.is_some();
        if counted && self.runtime.terminations.enter_gate() {
            self.session_became_busy();
        }
        StatementGateGuard {
            engine: self,
            _guard: guard,
            counted,
        }
    }
}
