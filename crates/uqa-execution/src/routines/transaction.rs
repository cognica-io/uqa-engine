//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

/// Identity used only to match nested procedural command scopes. It never grants access to session state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RoutineSessionId(pub usize);
use std::cell::RefCell;

thread_local! {
    static ROUTINE_TRANSACTION_STACK: RefCell<Vec<RoutineTransactionContext>> = const { RefCell::new(Vec::new()) };
    static DIRECT_ROUTINE_COMMAND_STACK: RefCell<Vec<RoutineSessionId>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy)]
struct RoutineTransactionContext {
    session: RoutineSessionId,
    nonatomic: bool,
}

pub struct RoutineTransactionGuard;

pub struct DirectRoutineCommandGuard {
    session: RoutineSessionId,
}

impl RoutineTransactionGuard {
    pub fn enter(session: RoutineSessionId, nonatomic: bool) -> Self {
        ROUTINE_TRANSACTION_STACK.with(|stack| {
            stack
                .borrow_mut()
                .push(RoutineTransactionContext { session, nonatomic });
        });
        Self
    }
}

impl Drop for RoutineTransactionGuard {
    fn drop(&mut self) {
        ROUTINE_TRANSACTION_STACK.with(|stack| {
            let removed = stack.borrow_mut().pop();
            debug_assert!(removed.is_some(), "routine transaction stack underflow");
        });
    }
}

impl DirectRoutineCommandGuard {
    pub fn enter(session: RoutineSessionId) -> Self {
        DIRECT_ROUTINE_COMMAND_STACK.with(|stack| stack.borrow_mut().push(session));
        Self { session }
    }
}

impl Drop for DirectRoutineCommandGuard {
    fn drop(&mut self) {
        DIRECT_ROUTINE_COMMAND_STACK.with(|stack| {
            let removed = stack.borrow_mut().pop();
            debug_assert_eq!(
                removed,
                Some(self.session),
                "direct routine command stack mismatch"
            );
        });
    }
}

pub fn routine_transaction_control_allowed(session: RoutineSessionId) -> bool {
    ROUTINE_TRANSACTION_STACK.with(|stack| {
        stack
            .borrow()
            .last()
            .is_some_and(|context| context.session == session && context.nonatomic)
    })
}

pub fn nonatomic_routine_entry_allowed(session: RoutineSessionId, nested_statement: bool) -> bool {
    if !nested_statement {
        return true;
    }
    routine_transaction_control_allowed(session)
        && DIRECT_ROUTINE_COMMAND_STACK
            .with(|stack| stack.borrow().last().is_some_and(|entry| *entry == session))
}
