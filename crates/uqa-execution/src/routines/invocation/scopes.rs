//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine volatility, definer nesting, and scoped caller-state restoration.
use super::context::RoutineInvocationSession;
use std::cell::{Cell, RefCell};
use uqa_sql::{
    ast::{CreateFunction, FunctionVolatility},
    SQLError,
};
thread_local! {
    static ROUTINE_VOLATILITY_STACK: RefCell<Vec<FunctionVolatility>> = const { RefCell::new(Vec::new()) };
    static SECURITY_DEFINER_DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub fn active_routine_reads_command_overlay() -> Option<bool> {
    ROUTINE_VOLATILITY_STACK.with(|stack| {
        stack
            .borrow()
            .last()
            .map(|volatility| *volatility == FunctionVolatility::Volatile)
    })
}

struct RoutineVolatilityGuard;

struct SecurityDefinerGuard {
    active: bool,
}

impl RoutineVolatilityGuard {
    fn enter(volatility: FunctionVolatility) -> Self {
        ROUTINE_VOLATILITY_STACK.with(|stack| stack.borrow_mut().push(volatility));
        Self
    }
}

impl Drop for RoutineVolatilityGuard {
    fn drop(&mut self) {
        ROUTINE_VOLATILITY_STACK.with(|stack| {
            let removed = stack.borrow_mut().pop();
            debug_assert!(removed.is_some(), "routine volatility stack underflow");
        });
    }
}

impl SecurityDefinerGuard {
    fn enter(active: bool) -> Self {
        if active {
            SECURITY_DEFINER_DEPTH.with(|depth| depth.set(depth.get() + 1));
        }
        Self { active }
    }
}

impl Drop for SecurityDefinerGuard {
    fn drop(&mut self) {
        if self.active {
            SECURITY_DEFINER_DEPTH.with(|depth| {
                let current = depth.get();
                debug_assert!(current > 0, "security-definer depth underflow");
                depth.set(current.saturating_sub(1));
            });
        }
    }
}

pub fn security_definer_active() -> bool {
    SECURITY_DEFINER_DEPTH.with(Cell::get) > 0
}

pub(super) fn with_routine_context<T>(
    session: &dyn RoutineInvocationSession,
    definition: &CreateFunction,
    execute: impl FnOnce() -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    let mut guard = session.state_guard();
    let _volatility = RoutineVolatilityGuard::enter(definition.volatility);
    let _security_definer = SecurityDefinerGuard::enter(definition.security.security_definer);
    if definition.security.security_definer {
        session.set_current_user(&definition.owner);
    }
    for (name, value) in &definition.config {
        session.set_variable(name, value)?;
    }
    let result = execute();
    if result.is_ok() && !definition.security.security_definer {
        guard.preserve_current_user();
    }
    result
}
