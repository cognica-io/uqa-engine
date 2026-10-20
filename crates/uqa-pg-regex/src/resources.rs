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

    /// `PostgreSQL`'s stack-depth check, made where the engine recurses. `depth` is the stack in bytes used since the current operation entered the engine; it starts again at zero in an operation that a hook runs. Returning `true` fails the operation with `REG_ETOOBIG` ("regular expression is too complex"), as `PostgreSQL` does when its stack limit is reached.
    ///
    /// The engine itself fails any operation deeper than [`crate::MAX_STACK_DEPTH`], `PostgreSQL`'s default `max_stack_depth`, without calling this hook, so the hook only needs to refuse when it wants a lower limit. Every operation runs with stack for that depth and [`crate::STACK_DEPTH_SLOP`] beyond it: on the thread's own stack when it has that much, and otherwise on a separately allocated stack of the same thread. An operation whose thread lacks that headroom first runs on the thread's stack while its depth leaves [`crate::STACK_DEPTH_SLOP`] free; if it needs more, it is abandoned at that depth check like a canceled operation, every byte it reserved is released, and it runs again from the start on a separate stack, so the hooks also see the calls of the abandoned attempt. The hooks themselves run within the slop and must use far less stack than it.
    ///
    /// Under Emscripten the engine's calls run on the WebAssembly call stack, which can be neither measured nor switched. There `depth` is the upper bound on live engine frames across every operation on the thread, times a fixed charge per frame, so it does not start again at zero in an operation that a hook runs. The charge is calibrated so that [`crate::MAX_STACK_DEPTH`] corresponds to half of V8's default stack at the largest engine frame measured, 1493 frames, which leaves the other half to the embedding: every compilation and execution fails with `REG_ETOOBIG` before it can exhaust the call stack, whatever its pattern and input.
    fn stack_too_deep(&mut self, depth: usize) -> bool;
}

impl<T: RegexResources + ?Sized> RegexResources for &mut T {
    fn reserve(&mut self, bytes: usize) -> bool {
        (**self).reserve(bytes)
    }
    fn release(&mut self, bytes: usize) {
        (**self).release(bytes);
    }
    fn cancel_requested(&mut self) -> bool {
        (**self).cancel_requested()
    }
    fn stack_too_deep(&mut self, depth: usize) -> bool {
        (**self).stack_too_deep(depth)
    }
}
