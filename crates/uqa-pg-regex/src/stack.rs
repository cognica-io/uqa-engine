//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stack headroom for the engine's recursion.
//!
//! `PostgreSQL`'s engine recurses on the C stack and checks its depth where it recurses: `stack_is_too_deep` compares the stack in use with `max_stack_depth`, which `PostgreSQL` keeps at least `STACK_DEPTH_SLOP` below the kernel's stack limit so that the frames beyond the last check always fit. A thread here may have less stack than that, so every compilation and execution runs where [`MAX_STACK_DEPTH`] plus [`STACK_DEPTH_SLOP`] are available: on the current stack when it has that much, and otherwise on a separately allocated stack of the same thread.
//!
//! An operation whose stack lacks that headroom but still has a useful amount runs there first, limited to the depth that leaves [`STACK_DEPTH_SLOP`] free. If it needs more, the engine abandons it at that depth check as it abandons a canceled operation, and the operation runs again from the start on a separate stack. Ordinary operations on threads with a few megabytes of stack therefore never allocate one, and no result depends on the thread's stack.

/// `PostgreSQL`'s default `max_stack_depth` of 2 MB: the deepest engine stack, measured from the start of an operation, that an operation may use.
///
/// The engine checks its depth where it recurses. Beyond this depth an operation fails with `REG_ETOOBIG` ("regular expression is too complex"), as `PostgreSQL`'s does at its default limit, without consulting [`crate::RegexResources::stack_too_deep`], which may impose a lower limit. Every operation runs with stack for this depth and [`STACK_DEPTH_SLOP`] beyond it, so any limit up to it is safe on every thread.
pub const MAX_STACK_DEPTH: usize = 2 * 1024 * 1024;

/// `PostgreSQL`'s `STACK_DEPTH_SLOP`: stack kept free beyond the deepest point the engine has checked, for the frames it does not check, the wrapper's callbacks and the resource hooks they call.
pub const STACK_DEPTH_SLOP: usize = 512 * 1024;

/// Stack the wrapper uses outside the engine's measurement: between measuring the remaining stack and entering the engine, and for switching to a separate stack.
#[cfg(not(target_family = "wasm"))]
const ENTRY_RESERVE: usize = 64 * 1024;

/// The least depth worth attempting on the current stack; ordinary operations need well under it.
#[cfg(not(target_family = "wasm"))]
const MIN_ATTEMPT_DEPTH: usize = 64 * 1024;

/// A separately allocated stack: the full depth and slop, the wrapper's entry and the switch itself.
#[cfg(not(target_family = "wasm"))]
const SEGMENT_SIZE: usize = MAX_STACK_DEPTH + STACK_DEPTH_SLOP + 2 * ENTRY_RESERVE;

/// The outcome of one attempt at an engine operation.
pub(crate) enum Attempt<T> {
    /// The operation finished, successfully or with an error.
    Finished(T),
    /// The engine needed more stack than the attempt's depth limit allowed.
    Restart,
}

impl<T> Attempt<T> {
    pub(crate) fn map<U>(self, finish: impl FnOnce(T) -> U) -> Attempt<U> {
        match self {
            Self::Finished(outcome) => Attempt::Finished(finish(outcome)),
            Self::Restart => Attempt::Restart,
        }
    }
}

/// Run one engine operation with stack headroom. `attempt` receives the depth its engine stack may reach, at most [`MAX_STACK_DEPTH`], and must leave nothing behind when it asks for a restart.
///
/// Returns `None` when the operation needs more stack than the thread can provide, which happens only on a target whose stack `stacker` cannot switch; the engine reports that as `REG_ETOOBIG`, as `PostgreSQL` does at its stack limit.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn run<T>(mut attempt: impl FnMut(usize) -> Attempt<T>) -> Option<T> {
    let depth = usable_depth();
    // An attempt allowed the full depth is never restarted: the engine fails beyond MAX_STACK_DEPTH instead.
    if depth >= MIN_ATTEMPT_DEPTH {
        if let Attempt::Finished(outcome) = attempt(depth) {
            return Some(outcome);
        }
    }
    stacker::grow(SEGMENT_SIZE, || match attempt(usable_depth()) {
        Attempt::Finished(outcome) => Some(outcome),
        Attempt::Restart => None,
    })
}

/// WebAssembly runs the engine's calls on its own call stack, which can be neither measured nor switched. The depth the engine measures there covers only its stack in linear memory, and the operation runs where it is.
#[cfg(target_family = "wasm")]
pub(crate) fn run<T>(mut attempt: impl FnMut(usize) -> Attempt<T>) -> Option<T> {
    match attempt(MAX_STACK_DEPTH) {
        Attempt::Finished(outcome) => Some(outcome),
        Attempt::Restart => None,
    }
}

/// The depth an attempt on the current stack may reach while leaving the slop and the wrapper's entry free, at most [`MAX_STACK_DEPTH`]; zero when the remaining stack is unknown.
#[cfg(not(target_family = "wasm"))]
fn usable_depth() -> usize {
    stacker::remaining_stack()
        .map_or(0, |remaining| {
            remaining.saturating_sub(STACK_DEPTH_SLOP + ENTRY_RESERVE)
        })
        .min(MAX_STACK_DEPTH)
}
