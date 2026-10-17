//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The caller's policy for the engine's memory, cancellation and stack checks.

/// Resource hooks for one compiled expression, its executions and its release.
///
/// The engine calls these hooks synchronously on the thread that runs the operation. A hook may panic: the operation stops at its next C frame boundary, frees its memory and the wrapper resumes the panic. A panic raised while the thread is already unwinding, which can happen only when an expression is dropped during unwinding, is discarded after all memory is freed, because a second panic would abort the process. A hook may itself compile and execute other expressions with other hooks.
///
/// UQA's shared memory allowances back [`Self::reserve`] and [`Self::release`] directly: grow one `MemoryReservation` lease by the requested bytes, refusing when the allowance is exhausted, and split released bytes off the lease and drop them. `ProductionControl::check_cancellation` backs [`Self::cancel_requested`].
pub trait RegexResources {
    /// Admit `bytes` more memory before it is allocated. Every chunk is charged with its header, and the compiled expression's memory context is charged when compilation starts. Returning `false` makes the allocation fail as an allocation failure does in `PostgreSQL`: an engine allocation then reports `REG_ESPACE` ("out of memory"), and the wrapper's pattern conversion and memory context, which correspond to `palloc` and context creation in `RE_compile_and_cache`, report [`crate::RegexError::OutOfMemory`]. A call that panics has admitted nothing.
    fn reserve(&mut self, bytes: usize) -> bool;

    /// Return `bytes` that an earlier [`Self::reserve`] admitted, after their memory has been freed. Over the life of a compiled expression the released total equals the reserved total.
    fn release(&mut self, bytes: usize);

    /// `PostgreSQL`'s interrupt check, made where the engine calls `CHECK_FOR_INTERRUPTS()` during compilation and matching. Returning `true` stops the operation with [`crate::RegexError::Cancelled`]. Releasing a compiled expression is never canceled.
    fn cancel_requested(&mut self) -> bool;

    /// `PostgreSQL`'s stack-depth check, made where the engine recurses. `depth` is the stack in bytes used since the current operation entered the engine; it starts again at zero in an operation that a hook runs, so a limit on the thread's whole stack must add the caller's own depth. Returning `true` fails the operation with `REG_ETOOBIG` ("regular expression is too complex"), as `PostgreSQL` does when its stack limit is reached.
    ///
    /// On `wasm32` targets, `depth` measures only the stack in linear memory, which holds the locals whose address is taken. The engine's calls themselves use the WebAssembly call stack, which cannot be measured; in an optimized build `depth` stays constant while the recursion deepens, so this check does not bound the engine's recursion there, and an embedding must keep patterns from exhausting the call stack by other means.
    fn stack_too_deep(&mut self, depth: usize) -> bool;
}
