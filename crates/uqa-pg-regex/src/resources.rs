//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The caller's policy for the engine's memory, cancellation and stack checks.

/// Resource hooks for one compiled expression, its executions and its release.
///
/// The engine calls these hooks synchronously on the thread that runs the operation. A hook may panic; the engine then stops at its next C frame boundary, frees the operation's chunks and the wrapper resumes the panic. A hook may itself compile and execute other expressions with other hooks.
///
/// UQA's shared memory allowances back [`Self::reserve`] and [`Self::release`] directly: grow one `MemoryReservation` lease by the requested bytes, refusing when the allowance is exhausted, and split released bytes off the lease and drop them. `ProductionControl::check_cancellation` backs [`Self::cancel_requested`].
pub trait RegexResources {
    /// Admit `bytes` more memory before the engine allocates them. Every chunk is charged with its header, and the compiled expression's own context is charged when compilation starts. Returning `false` makes the allocation fail, and the engine reports `REG_ESPACE` ("out of memory") as `PostgreSQL` does when an allocation fails.
    fn reserve(&mut self, bytes: usize) -> bool;

    /// Return `bytes` that an earlier [`Self::reserve`] admitted, after their memory has been freed. Over the life of a compiled expression the released total equals the reserved total.
    fn release(&mut self, bytes: usize);

    /// `PostgreSQL`'s interrupt check, made where the engine calls `CHECK_FOR_INTERRUPTS()` during compilation and matching. Returning `true` stops the operation with [`crate::RegexError::Cancelled`]. Releasing a compiled expression is never canceled.
    fn cancel_requested(&mut self) -> bool;

    /// `PostgreSQL`'s stack-depth check. `depth` is the stack in bytes used since the current operation entered the engine. Returning `true` fails the operation with `REG_ETOOBIG` ("regular expression is too complex"), as `PostgreSQL` does when its stack limit is reached.
    fn stack_too_deep(&mut self, depth: usize) -> bool;
}
