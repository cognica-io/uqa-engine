//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native stack and frame-count limits for routine entry.
use super::context::RoutineInvocationSession;
use std::cell::Cell;
use uqa_sql::SQLError;
thread_local! {
    static CALL_DEPTH: Cell<usize> = const { Cell::new(0) };
    static STACK_BASE: Cell<usize> = const { Cell::new(0) };
}
/// Native stack budget for nested routine calls, measured from the
/// outermost routine entry. The `PostgreSQL` `max_stack_depth`
/// setting plays the same role (default 2MB there); this budget is
/// sized so the guard
/// always fires before a 2MB thread stack (the Rust test-runner
/// default) is exhausted, even in debug builds.
const STACK_BYTE_BUDGET: usize = 1_000_000;

/// Approximate current stack position.
#[inline(never)]
fn stack_marker() -> usize {
    let marker = 0u8;
    std::ptr::from_ref(&marker) as usize
}

fn stack_depth_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "54001".into(),
        message: "stack depth limit exceeded".into(),
    }
}

/// RAII guard for the user-routine nesting caps: a configurable
/// frame-count limit plus a native stack-byte budget.
pub(super) struct DepthGuard;

impl DepthGuard {
    pub(super) fn enter(session: &dyn RoutineInvocationSession) -> Result<Self, SQLError> {
        let depth = CALL_DEPTH.get();
        if depth == 0 {
            STACK_BASE.set(stack_marker());
        } else if STACK_BASE.get().abs_diff(stack_marker()) > STACK_BYTE_BUDGET {
            return Err(stack_depth_error());
        }
        if depth >= session.depth_limit() {
            return Err(stack_depth_error());
        }
        CALL_DEPTH.set(depth + 1);
        Ok(Self)
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        CALL_DEPTH.set(CALL_DEPTH.get().saturating_sub(1));
    }
}
