//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A compiled `PostgreSQL` regular expression and its executions.
#![allow(unsafe_code)] // Compiled state is owned through pointers returned by the C shim.

use std::any::Any;
use std::ffi::c_long;
use std::panic::{self, AssertUnwindSafe};
use std::ptr::{self, NonNull};

use crate::collation::RegexCollation;
use crate::error::RegexError;
use crate::ffi::{self, RegexT, Thrown};
use crate::flags::{CompileFlags, REG_ESPACE, REG_INVARG, REG_NOMATCH, REG_OKAY};
use crate::memory::{self, MemoryContext, OwnedContext};
use crate::operation::Operation;
use crate::resources::RegexResources;
use crate::span::MatchSpan;

/// A pattern compiled by `PostgreSQL`'s `pg_regcomp`.
///
/// The compiled state lives in its own memory context, charged to the resources passed to [`Regex::compile`]; dropping the expression frees it with `pg_regfree` and releases every reserved byte. Executions allocate their working memory in a separate context that is empty again when [`Regex::exec`] returns.
///
/// An expression borrows its resources mutably for its whole life and is used by one thread at a time; it is neither `Send` nor `Sync`. Expressions on different threads are independent.
pub struct Regex<'r> {
    expression: NonNull<RegexT>,
    context: Option<OwnedContext>,
    resources: &'r mut dyn RegexResources,
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

impl<'r> Regex<'r> {
    /// Compile `pattern` with `flags` under `collation`, as `pg_regcomp` does for `PostgreSQL`'s SQL functions.
    ///
    /// The pattern is converted to code points in memory charged to `resources`, as `RE_compile_and_cache` converts it before compiling. Compilation failures return [`RegexError::Engine`]; `PostgreSQL` reports them as `invalid regular expression: <explanation>`.
    pub fn compile(
        pattern: &str,
        flags: CompileFlags,
        collation: RegexCollation,
        resources: &'r mut dyn RegexResources,
    ) -> Result<Self, RegexError> {
        let Some(context) = OwnedContext::new(resources) else {
            return Err(RegexError::Engine(REG_ESPACE));
        };
        // A hook called directly from Rust may panic as well; the context is freed before the panic continues.
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            compile_into(&context, pattern, flags, collation, &mut *resources)
        }));
        match outcome {
            Ok(Ok(expression)) => {
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
            Ok(Err(stop)) => {
                context.release(resources);
                Err(stop.resume())
            }
            Err(payload) => {
                context.release(resources);
                panic::resume_unwind(payload)
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
    /// Returns `Ok(true)` on a match and `Ok(false)` without one, with every slot unset. A `search_start` beyond the input finds no match, and `^` and lookbehind constraints see the input before `search_start`, as in `pg_regexec`. Engine failures return [`RegexError::Engine`]; `PostgreSQL` reports them as `regular expression failed: <explanation>`. An input longer than the engine's `long` offsets can represent fails with `REG_INVARG`.
    pub fn exec(
        &mut self,
        input: &[u32],
        search_start: usize,
        matches: &mut [MatchSpan],
    ) -> Result<bool, RegexError> {
        // pg_regexec rejects a NULL string even when it is empty.
        static EMPTY: [u32; 1] = [0];
        if c_long::try_from(input.len()).is_err() {
            return Err(RegexError::Engine(REG_INVARG));
        }
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
        // SAFETY: the context stays at this address until it is released below.
        unsafe { MemoryContext::initialize(&raw mut context) };
        let mut thrown = Thrown::default();
        let mut operation = Operation::new(&raw mut context, &mut *self.resources, true);
        // SAFETY: the expression is live, `data` holds `input.len()` code points and `pmatch` holds `matches.len()` slots for the duration of the call.
        let code = unsafe {
            ffi::uqa_pg_regex_execute(
                operation.as_raw(),
                self.expression.as_ptr(),
                data,
                input.len(),
                search_start,
                matches.len(),
                pmatch,
                &raw mut thrown,
            )
        };
        let stop = (code == ffi::THROWN).then(|| stopped(thrown, operation.take_panic()));
        // SAFETY: the C call has returned, so nothing refers to the execution's chunks any more.
        let leftover = unsafe { MemoryContext::free_all(&raw mut context) };
        if leftover > 0 {
            self.resources.release(leftover);
        }
        if let Some(stop) = stop {
            matches.fill(MatchSpan::UNSET);
            return Err(stop.resume());
        }
        match code {
            REG_OKAY => Ok(true),
            REG_NOMATCH => Ok(false),
            code => {
                matches.fill(MatchSpan::UNSET);
                Err(RegexError::Engine(code))
            }
        }
    }
}

impl Drop for Regex<'_> {
    fn drop(&mut self) {
        let Some(context) = self.context.take() else {
            return;
        };
        let mut thrown = Thrown::default();
        let mut operation = Operation::new(context.as_ptr(), &mut *self.resources, false);
        // SAFETY: the expression was compiled in this context and is freed exactly once.
        let code = unsafe {
            ffi::uqa_pg_regex_release(
                operation.as_raw(),
                self.expression.as_ptr(),
                &raw mut thrown,
            )
        };
        let stop = (code == ffi::THROWN).then(|| stopped(thrown, operation.take_panic()));
        // Every chunk the release did not reach is freed with the context.
        context.release(&mut *self.resources);
        if let Some(stop) = stop {
            // Release frees memory only, so only a panicking hook can stop it.
            let error = stop.resume();
            unreachable!("releasing a compiled expression failed: {error}");
        }
    }
}

fn compile_into(
    context: &OwnedContext,
    pattern: &str,
    flags: CompileFlags,
    collation: RegexCollation,
    resources: &mut dyn RegexResources,
) -> Result<NonNull<RegexT>, Stop> {
    let refused = || Stop::Error(RegexError::Engine(REG_ESPACE));
    let length = pattern.chars().count();
    // Like RE_compile_and_cache, allocate one slot more than the pattern needs.
    let bytes = length
        .checked_add(1)
        .and_then(|slots| slots.checked_mul(size_of::<u32>()))
        .ok_or_else(refused)?;
    // SAFETY: the context is initialized and owned by the caller.
    let buffer = unsafe { MemoryContext::allocate(context.as_ptr(), resources, bytes) }
        .ok_or_else(refused)?;
    let code_points = memory::code_point_slots(buffer);
    for (index, character) in pattern.chars().enumerate() {
        // SAFETY: the 16-byte aligned buffer holds `length + 1` u32 slots.
        unsafe { code_points.add(index).write(u32::from(character)) };
    }
    let mut thrown = Thrown::default();
    let mut result: *mut RegexT = ptr::null_mut();
    let mut operation = Operation::new(context.as_ptr(), resources, true);
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
        // The pattern buffer and every engine chunk are freed with the context.
        return Err(stopped(thrown, operation.take_panic()));
    }
    // SAFETY: the buffer is a live chunk of this context and the engine no longer refers to it.
    unsafe { MemoryContext::free(buffer, resources) };
    if code != REG_OKAY {
        return Err(Stop::Error(RegexError::Engine(code)));
    }
    Ok(NonNull::new(result).expect("pg_regcomp success provides the compiled expression"))
}

/// Map a failure reported through the C error frame.
fn stopped(thrown: Thrown, panic: Option<Box<dyn Any + Send + 'static>>) -> Stop {
    let error = match thrown.kind {
        ffi::THROW_CALLBACK => {
            return Stop::Panic(panic.expect("a callback failure records its panic"));
        }
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
    Stop::Error(error)
}

/// The engine's offsets are C `long` values.
const _: () = assert!(size_of::<MatchSpan>() == 2 * size_of::<c_long>());

#[cfg(test)]
mod tests;
