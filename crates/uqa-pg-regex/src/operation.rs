//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One engine call and the C callbacks that serve it.
//!
//! The wrapper passes a pointer to an [`Operation`] into each C operation function, and the shim passes it back to the callbacks below. Every hook call runs under `catch_unwind`: a panic is kept in the operation and reported to C, which leaves the engine through its error frame; the wrapper resumes the panic after the C call has returned.
#![allow(unsafe_code)] // The callbacks are exported to C and dereference the operation pointer C passes back.

use std::any::Any;
use std::ffi::{c_int, c_void};
use std::panic::{self, AssertUnwindSafe};
use std::ptr::{self, NonNull};

use crate::ffi::{CALLBACK_OK, CALLBACK_PANICKED, CALLBACK_REFUSED, CALLBACK_RESTART};
use crate::memory::MemoryContext;
use crate::resources::RegexResources;
use crate::stack::{self, MAX_STACK_DEPTH};

pub(crate) struct Operation<'a> {
    context: *mut MemoryContext,
    resources: &'a mut dyn RegexResources,
    depth_limit: usize,
    panic: Option<Box<dyn Any + Send + 'static>>,
}

/// A hook call that panicked; its payload is kept in the operation.
#[derive(Clone, Copy)]
struct Panicked;

impl<'a> Operation<'a> {
    /// An operation whose engine allocations go to `context` and whose engine stack may reach `depth_limit` bytes, at most [`MAX_STACK_DEPTH`], on the stack it runs on.
    pub(crate) fn new(
        context: *mut MemoryContext,
        resources: &'a mut dyn RegexResources,
        depth_limit: usize,
    ) -> Self {
        Self {
            context,
            resources,
            depth_limit,
            panic: None,
        }
    }

    /// The pointer handed to C for the duration of one operation function.
    pub(crate) fn as_raw(&mut self) -> *mut c_void {
        ptr::from_mut(self).cast::<c_void>()
    }

    /// The panic of a hook that stopped the operation, if any.
    pub(crate) fn take_panic(&mut self) -> Option<Box<dyn Any + Send + 'static>> {
        self.panic.take()
    }

    fn guard<T>(&mut self, call: impl FnOnce(&mut Self) -> T) -> Result<T, Panicked> {
        panic::catch_unwind(AssertUnwindSafe(|| call(&mut *self))).map_err(|payload| {
            self.panic = Some(payload);
            Panicked
        })
    }

    /// # Safety
    /// `operation` must be the pointer from [`Operation::as_raw`] of an operation that is still running a C operation function.
    unsafe fn from_raw<'b>(operation: *mut c_void) -> &'b mut Operation<'b> {
        &mut *operation.cast::<Operation<'b>>()
    }
}

/// The callback result for an outcome whose `true` value lets the engine continue.
fn status(outcome: Result<bool, Panicked>) -> c_int {
    match outcome {
        Ok(true) => CALLBACK_OK,
        Ok(false) => CALLBACK_REFUSED,
        Err(Panicked) => CALLBACK_PANICKED,
    }
}

/// Report an allocation through C's result slot and the callback status.
fn allocation_status(
    outcome: Result<Option<NonNull<u8>>, Panicked>,
    result: *mut *mut c_void,
) -> c_int {
    let pointer = match &outcome {
        Ok(Some(pointer)) => pointer.as_ptr().cast::<c_void>(),
        Ok(None) | Err(Panicked) => ptr::null_mut(),
    };
    // SAFETY: C passes a valid result slot for the duration of the call.
    unsafe { result.write(pointer) };
    status(outcome.map(|allocated| allocated.is_some()))
}

#[no_mangle]
extern "C" fn uqa_pg_regex_callback_allocate(
    operation: *mut c_void,
    size: usize,
    result: *mut *mut c_void,
) -> c_int {
    // SAFETY: C passes back the pointer of the operation it is serving.
    let operation = unsafe { Operation::from_raw(operation) };
    let outcome = operation.guard(|operation| {
        // SAFETY: the operation's context is initialized and alive while the C call runs.
        unsafe { MemoryContext::allocate(operation.context, operation.resources, size) }
    });
    allocation_status(outcome, result)
}

#[no_mangle]
extern "C" fn uqa_pg_regex_callback_reallocate(
    operation: *mut c_void,
    pointer: *mut c_void,
    size: usize,
    result: *mut *mut c_void,
) -> c_int {
    // SAFETY: C passes back the pointer of the operation it is serving.
    let operation = unsafe { Operation::from_raw(operation) };
    let Some(payload) = NonNull::new(pointer.cast::<u8>()) else {
        // repalloc of NULL is a defect in the caller; PostgreSQL would crash on it as well.
        eprintln!("uqa-pg-regex: repalloc_extended called with a NULL pointer");
        std::process::abort();
    };
    let outcome = operation.guard(|operation| {
        // SAFETY: the engine passes a live chunk that this wrapper allocated.
        unsafe { MemoryContext::reallocate(payload, operation.resources, size) }
    });
    allocation_status(outcome, result)
}

#[no_mangle]
extern "C" fn uqa_pg_regex_callback_free(operation: *mut c_void, pointer: *mut c_void) -> c_int {
    // SAFETY: C passes back the pointer of the operation it is serving.
    let operation = unsafe { Operation::from_raw(operation) };
    let Some(payload) = NonNull::new(pointer.cast::<u8>()) else {
        return CALLBACK_OK;
    };
    status(operation.guard(|operation| {
        // SAFETY: the engine passes a live chunk that this wrapper allocated.
        unsafe { MemoryContext::free(payload, operation.resources) };
        true
    }))
}

#[no_mangle]
extern "C" fn uqa_pg_regex_callback_interrupt(operation: *mut c_void) -> c_int {
    // SAFETY: C passes back the pointer of the operation it is serving.
    let operation = unsafe { Operation::from_raw(operation) };
    status(operation.guard(|operation| !operation.resources.cancel_requested()))
}

#[no_mangle]
extern "C" fn uqa_pg_regex_callback_stack(operation: *mut c_void, measure: usize) -> c_int {
    // SAFETY: C passes back the pointer of the operation it is serving.
    let operation = unsafe { Operation::from_raw(operation) };
    let depth = stack::depth(measure);
    if depth > MAX_STACK_DEPTH {
        // PostgreSQL's default max_stack_depth: the engine fails with REG_ETOOBIG.
        return CALLBACK_REFUSED;
    }
    if depth > operation.depth_limit {
        // This stack cannot go deeper with STACK_DEPTH_SLOP to spare; the operation runs again on a larger one.
        return CALLBACK_RESTART;
    }
    status(operation.guard(|operation| !operation.resources.stack_too_deep(depth)))
}

#[cfg(test)]
mod tests;
