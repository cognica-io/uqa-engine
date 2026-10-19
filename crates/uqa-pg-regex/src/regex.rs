//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A compiled `PostgreSQL` regular expression and its executions.
#![allow(unsafe_code)] // Compiled state is owned through pointers returned by the C shim.

use std::any::Any;
use std::ffi::{c_int, c_long};
use std::panic::{self, AssertUnwindSafe};
use std::ptr::{self, NonNull};

use crate::collation::RegexCollation;
use crate::error::RegexError;
use crate::ffi::{self, RegexT, Thrown};
use crate::flags::{CompileFlags, REG_ETOOBIG, REG_INVARG, REG_NOMATCH, REG_OKAY};
use crate::memory::{self, MemoryContext, OwnedContext};
use crate::operation::Operation;
use crate::resources::RegexResources;
use crate::span::MatchSpan;
use crate::stack::{self, Attempt};

/// A pattern compiled by `PostgreSQL`'s `pg_regcomp`.
///
/// The compiled state lives in its own memory context, charged to the resources passed to [`Regex::compile`]; dropping the expression deletes that context and releases every reserved byte, as `RE_compile_and_cache` deletes the context of an expression it discards. Executions allocate their working memory in a separate context that is empty again when [`Regex::exec`] returns. Compilation and execution run with the stack headroom described at [`crate::MAX_STACK_DEPTH`].
///
/// An expression borrows its resources mutably for its whole life and is used by one thread at a time; it is neither `Send` nor `Sync`. Expressions on different threads are independent.
pub struct Regex<'r, R: RegexResources + ?Sized = dyn RegexResources + 'r> {
    expression: NonNull<RegexT>,
    context: Option<OwnedContext>,
    resources: &'r mut R,
    subexpressions: usize,
}

/// How an operation stopped when it did not return an engine code.
enum Stop {
    Error(RegexError),
    Panic(Box<dyn Any + Send + 'static>),
}

impl Stop {
    fn resume(self) -> RegexError {
        match self {
            Self::Error(error) => error,
            Self::Panic(payload) => std::panic::resume_unwind(payload),
        }
    }
}

impl<'r, R: RegexResources + ?Sized> Regex<'r, R> {
    /// Compile `pattern` with `flags` under `collation`, as `pg_regcomp` does for `PostgreSQL`'s SQL functions.
    ///
    /// Like `RE_compile_and_cache`, the wrapper converts the pattern into a buffer of one code point slot per pattern byte plus one and gives the compiled expression its own memory context, both in memory charged to `resources`. Their requests fail as `palloc` and context creation fail in `PostgreSQL`: a pattern of 268,435,455 bytes or more with [`RegexError::InvalidAllocationSize`] before anything is allocated, and a refused reservation with [`RegexError::OutOfMemory`].
    ///
    /// The engine's own failures return [`RegexError::Code`], which `PostgreSQL` reports as `invalid regular expression: <explanation>`, including `REG_ESPACE` when `resources` refuse an engine allocation and `REG_ETOOBIG` beyond [`crate::MAX_STACK_DEPTH`] or a lower limit of `resources`; [`RegexError::Cancelled`] when `resources` request cancellation; and [`RegexError::InvalidAllocationSize`] or [`RegexError::InvalidAllocationProduct`] when the engine requests an invalid size.
    pub fn compile(
        pattern: &str,
        flags: CompileFlags,
        collation: RegexCollation,
        resources: &'r mut R,
    ) -> Result<Self, RegexError> {
        let bytes = pattern_buffer_bytes(pattern.len())?;
        let Some(context) = OwnedContext::new(&mut &mut *resources) else {
            return Err(RegexError::OutOfMemory);
        };
        // Cover the complete operation, including a restart and the stack switch. Every failure releases the context before a caller's panic continues.
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            stack::run(|depth_limit| {
                compile_into(
                    &context,
                    pattern,
                    bytes,
                    flags,
                    collation,
                    &mut &mut *resources,
                    depth_limit,
                )
            })
            .map_err(Stop::Error)?
            .unwrap_or(Err(Stop::Error(RegexError::Code(REG_ETOOBIG))))
        }))
        .unwrap_or_else(|payload| Err(Stop::Panic(payload)));
        match outcome {
            Ok(expression) => {
                // SAFETY: a successful compilation returns a live regex_t.
                let subexpressions =
                    unsafe { ffi::uqa_pg_regex_subexpressions(expression.as_ptr()) };
                Ok(Self {
                    expression,
                    context: Some(context),
                    resources,
                    subexpressions,
                })
            }
            Err(stop) => {
                context.release(&mut &mut *resources);
                Err(stop.resume())
            }
        }
    }

    /// The number of capturing subexpressions, `PostgreSQL`'s `re_nsub`.
    pub fn subexpression_count(&self) -> usize {
        self.subexpressions
    }

    /// Bytes currently reserved for the compiled state, its context included.
    pub fn reserved_bytes(&self) -> usize {
        self.context
            .as_ref()
            .map_or(0, OwnedContext::reserved_bytes)
    }

    /// Search `input` from `search_start` with `pg_regexec`, writing one span per slot of `matches`: slot 0 is the whole match and slot `n` subexpression `n`.
    ///
    /// Returns `Ok(true)` on a match and `Ok(false)` without one, with every slot unset. A `search_start` beyond the input finds no match, and `^` and lookbehind constraints see the input before `search_start`, as in `pg_regexec`. Engine failures return [`RegexError::Code`]; `PostgreSQL` reports them as `regular expression failed: <explanation>`. An input longer than the engine's `long` offsets can represent fails with `REG_INVARG`.
    pub fn exec(
        &mut self,
        input: &[u32],
        search_start: usize,
        matches: &mut [MatchSpan],
    ) -> Result<bool, RegexError> {
        exec_expression(
            self.expression,
            &mut &mut *self.resources,
            input,
            search_start,
            matches,
        )
    }

    /// Transfer the compiled context and its existing reservation to a retained owner.
    /// The transfer moves the resource charge, without releasing or readmitting it. The
    /// callback must leave the borrowed hooks able to release any bytes it does not transfer.
    /// Each later execution receives its own scratch/cancellation hooks.
    pub fn into_retained<T: RegexResources>(
        mut self,
        transfer: impl FnOnce(&mut R) -> T,
    ) -> RetainedRegex<T> {
        let resources = transfer(self.resources);
        RetainedRegex {
            expression: self.expression,
            context: self.context.take(),
            resources,
            subexpressions: self.subexpressions,
        }
    }
}

impl<R: RegexResources + ?Sized> Drop for Regex<'_, R> {
    fn drop(&mut self) {
        let Some(context) = self.context.take() else {
            return;
        };
        // Deleting the context frees the compiled expression, as RE_compile_and_cache deletes the context of an expression it discards instead of calling pg_regfree; no engine code runs.
        let released = panic::catch_unwind(AssertUnwindSafe(|| {
            context.release(&mut &mut *self.resources);
        }));
        // All memory is freed before the release hook runs. During unwinding from another panic, a second panic would abort the process, so the hook's panic ends here.
        if let Err(payload) = released {
            if !std::thread::panicking() {
                panic::resume_unwind(payload);
            }
        }
    }
}

/// A compiled expression with its owned resource reservation. Matching uses only the
/// invoking call's hooks; dropping the expression releases the original reservation.
/// A retained expression may move between threads but is never executed concurrently.
pub struct RetainedRegex<R: RegexResources> {
    expression: NonNull<RegexT>,
    context: Option<OwnedContext>,
    resources: R,
    subexpressions: usize,
}

// SAFETY: all mutable compiled allocations belong to the heap context, whose address
// is stable. Compilation retains no stack, thread-local operation frame or resource
// pointer. Shared character-class vectors are immutable process-lifetime allocations.
// Execution requires exclusive access and creates its frame and scratch on its caller.
unsafe impl<R: RegexResources + Send> Send for RetainedRegex<R> {}

impl<R: RegexResources> RetainedRegex<R> {
    /// Number of capturing subexpressions in the retained pattern.
    pub fn subexpression_count(&self) -> usize {
        self.subexpressions
    }

    /// Bytes charged to the retained compilation owner.
    pub fn reserved_bytes(&self) -> usize {
        self.context
            .as_ref()
            .map_or(0, OwnedContext::reserved_bytes)
    }

    /// Search with the current invocation's scratch allowance and cancellation hooks.
    pub fn exec(
        &mut self,
        input: &[u32],
        search_start: usize,
        matches: &mut [MatchSpan],
        resources: &mut dyn RegexResources,
    ) -> Result<bool, RegexError> {
        exec_expression(self.expression, resources, input, search_start, matches)
    }
}

impl<R: RegexResources> Drop for RetainedRegex<R> {
    fn drop(&mut self) {
        if let Some(context) = self.context.take() {
            let released = panic::catch_unwind(AssertUnwindSafe(|| {
                context.release(&mut self.resources);
            }));
            if let Err(payload) = released {
                if !std::thread::panicking() {
                    panic::resume_unwind(payload);
                }
            }
        }
    }
}

fn exec_expression(
    expression: NonNull<RegexT>,
    resources: &mut dyn RegexResources,
    input: &[u32],
    search_start: usize,
    matches: &mut [MatchSpan],
) -> Result<bool, RegexError> {
    if c_long::try_from(input.len()).is_err() {
        return Err(RegexError::Code(REG_INVARG));
    }
    let outcome = stack::run(|depth_limit| {
        execute(
            expression,
            &mut *resources,
            input,
            search_start,
            &mut *matches,
            depth_limit,
        )
    });
    let result = match outcome {
        Ok(Some(Ok(REG_OKAY))) => return Ok(true),
        Ok(Some(Ok(REG_NOMATCH))) => Ok(false),
        Ok(Some(Ok(code))) => Err(Stop::Error(RegexError::Code(code))),
        Ok(Some(Err(stop))) => Err(stop),
        Ok(None) => Err(Stop::Error(RegexError::Code(REG_ETOOBIG))),
        Err(error) => Err(Stop::Error(error)),
    };
    matches.fill(MatchSpan::UNSET);
    result.map_err(Stop::resume)
}

/// The size of `RE_compile_and_cache`'s code point buffer for a pattern of `length` bytes, which `palloc` validates before allocating.
fn pattern_buffer_bytes(length: usize) -> Result<usize, RegexError> {
    // A string's length is at most `isize::MAX`, so one more slot cannot overflow.
    let slots = length + 1;
    let bytes = slots
        .checked_mul(size_of::<u32>())
        .ok_or(RegexError::InvalidAllocationProduct(
            slots,
            size_of::<u32>(),
        ))?;
    if bytes > memory::MAX_ALLOC_SIZE {
        return Err(RegexError::InvalidAllocationSize(bytes));
    }
    Ok(bytes)
}

fn compile_into(
    context: &OwnedContext,
    pattern: &str,
    bytes: usize,
    flags: CompileFlags,
    collation: RegexCollation,
    resources: &mut dyn RegexResources,
    depth_limit: usize,
) -> Attempt<Result<NonNull<RegexT>, Stop>> {
    // SAFETY: the context is initialized and owned by the caller.
    let Some(buffer) = (unsafe { MemoryContext::allocate(context.as_ptr(), resources, bytes) })
    else {
        return Attempt::Finished(Err(Stop::Error(RegexError::OutOfMemory)));
    };
    let code_points = memory::code_point_slots(buffer);
    let mut length = 0;
    for character in pattern.chars() {
        // SAFETY: the 16-byte aligned buffer holds one slot per pattern byte plus one, at least one per character.
        unsafe { code_points.add(length).write(u32::from(character)) };
        length += 1;
    }
    let mut thrown = Thrown::default();
    let mut result: *mut RegexT = ptr::null_mut();
    let mut operation = Operation::new(context.as_ptr(), resources, depth_limit);
    // SAFETY: the buffer holds `length` code points; `result` and `thrown` are valid for writes.
    let code = unsafe {
        ffi::uqa_pg_regex_compile(
            operation.as_raw(),
            code_points,
            length,
            flags.bits(),
            collation.token(),
            &raw mut result,
            &raw mut thrown,
        )
    };
    if code == ffi::THROWN {
        let stopped = stopped(thrown, operation.take_panic());
        if let Attempt::Restart = stopped {
            // SAFETY: the C call has returned, so nothing refers to the abandoned attempt's chunks any more.
            unsafe { context.reset(resources) };
        }
        // Otherwise the pattern buffer and every engine chunk are freed with the context.
        return stopped.map(Err);
    }
    // SAFETY: the buffer is a live chunk of this context and the engine no longer refers to it.
    unsafe { MemoryContext::free(buffer, resources) };
    if code != REG_OKAY {
        return Attempt::Finished(Err(Stop::Error(RegexError::Code(code))));
    }
    Attempt::Finished(Ok(
        NonNull::new(result).expect("pg_regcomp success provides the compiled expression")
    ))
}

/// One attempt at `pg_regexec`, with working memory in a context of its own that is empty again when the attempt returns.
fn execute(
    expression: NonNull<RegexT>,
    resources: &mut dyn RegexResources,
    input: &[u32],
    search_start: usize,
    matches: &mut [MatchSpan],
    depth_limit: usize,
) -> Attempt<Result<c_int, Stop>> {
    // pg_regexec rejects a NULL string even when it is empty.
    static EMPTY: [u32; 1] = [0];
    matches.fill(MatchSpan::UNSET);
    let data = if input.is_empty() {
        EMPTY.as_ptr()
    } else {
        input.as_ptr()
    };
    let pmatch = if matches.is_empty() {
        ptr::null_mut()
    } else {
        matches.as_mut_ptr()
    };
    let mut context = MemoryContext::unlinked();
    // SAFETY: the context stays at this address until it is freed below.
    unsafe { MemoryContext::initialize(&raw mut context) };
    let mut thrown = Thrown::default();
    let mut operation = Operation::new(&raw mut context, resources, depth_limit);
    // SAFETY: the expression is live, `data` holds `input.len()` code points and `pmatch` holds `matches.len()` slots for the duration of the call.
    let code = unsafe {
        ffi::uqa_pg_regex_execute(
            operation.as_raw(),
            expression.as_ptr(),
            data,
            input.len(),
            search_start,
            matches.len(),
            pmatch,
            &raw mut thrown,
        )
    };
    let stopped = (code == ffi::THROWN).then(|| stopped(thrown, operation.take_panic()));
    // SAFETY: the C call has returned, so nothing refers to the execution's chunks any more.
    let leftover = unsafe { MemoryContext::free_all(&raw mut context) };
    if leftover > 0 {
        resources.release(leftover);
    }
    match stopped {
        Some(stopped) => stopped.map(Err),
        None => Attempt::Finished(Ok(code)),
    }
}

/// Map a failure reported through the C error frame.
fn stopped(thrown: Thrown, panic: Option<Box<dyn Any + Send + 'static>>) -> Attempt<Stop> {
    let error = match thrown.kind {
        ffi::THROW_CALLBACK => {
            return Attempt::Finished(Stop::Panic(
                panic.expect("a callback failure records its panic"),
            ));
        }
        ffi::THROW_STACK => return Attempt::Restart,
        ffi::THROW_CANCELED => RegexError::Cancelled,
        ffi::THROW_ALLOC_SIZE => RegexError::InvalidAllocationSize(thrown.first),
        ffi::THROW_ALLOC_PRODUCT => {
            RegexError::InvalidAllocationProduct(thrown.first, thrown.second)
        }
        ffi::THROW_OUT_OF_MEMORY => RegexError::OutOfMemory,
        ffi::THROW_LOCALE => RegexError::UnknownLocale(
            u32::try_from(thrown.first).expect("locale tokens are 32-bit OIDs"),
        ),
        kind => unreachable!("the C shim reports only known failure kinds, not {kind}"),
    };
    Attempt::Finished(Stop::Error(error))
}

/// The engine's offsets are C `long` values.
const _: () = assert!(size_of::<MatchSpan>() == 2 * size_of::<c_long>());

#[cfg(test)]
mod tests;
