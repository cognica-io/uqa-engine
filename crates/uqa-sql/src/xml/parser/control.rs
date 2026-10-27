//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native parser allocations borrow one operation's allowance and cancellation owners.

use std::{
    ffi::{c_int, c_void},
    marker::PhantomData,
    sync::Once,
};
use uqa_core::{
    memory::{MemoryError, MemoryReservation, ProductionControl},
    ValueRetentionError,
};

unsafe extern "C" {
    fn uqa_sql_xml_initialize();
    fn uqa_sql_xml_enter(control: *mut NativeControl) -> *mut NativeControl;
    fn uqa_sql_xml_leave(previous: *mut NativeControl);
}

#[repr(C)]
pub(super) struct NativeControl {
    context: *mut c_void,
    reserve: unsafe extern "C" fn(*mut c_void, usize) -> c_int,
    release: unsafe extern "C" fn(*mut c_void, usize),
}

pub(super) struct State<'a> {
    control: ProductionControl<'a>,
    memory: Option<MemoryReservation>,
    bytes: usize,
    pub error: Option<ValueRetentionError>,
}

impl<'a> State<'a> {
    pub fn new(control: ProductionControl<'a>) -> Self {
        Self {
            control,
            memory: control.empty_reservation(),
            bytes: 0,
            error: None,
        }
    }

    pub fn native(&mut self) -> NativeControl {
        NativeControl {
            context: std::ptr::from_mut(self).cast(),
            reserve,
            release,
        }
    }

    fn reserve(&mut self, bytes: usize) -> Result<(), ValueRetentionError> {
        #[cfg(test)]
        tests::allocation(bytes)?;
        self.control.check()?;
        let total = self
            .bytes
            .checked_add(bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        if let Some(memory) = &mut self.memory {
            memory.grow(bytes)?;
        }
        self.bytes = total;
        Ok(())
    }
}

#[cfg(test)]
impl Drop for State<'_> {
    fn drop(&mut self) {
        assert_eq!(
            self.bytes, 0,
            "native XML allocation outlived its operation"
        );
    }
}

unsafe extern "C" fn reserve(context: *mut c_void, bytes: usize) -> c_int {
    // SAFETY: the synchronous native scope borrows the live State, and calls are sequential on this thread.
    let state = unsafe { &mut *context.cast::<State<'_>>() };
    if state.error.is_some() {
        return -1;
    }
    match state.reserve(bytes) {
        Ok(()) => 0,
        Err(error) => {
            state.error = Some(error);
            -1
        }
    }
}

unsafe extern "C" fn release(context: *mut c_void, bytes: usize) {
    // SAFETY: each native block records its original live scope and exact admitted size; every block is freed before leaving that scope.
    let state = unsafe { &mut *context.cast::<State<'_>>() };
    state.bytes -= bytes;
    if let Some(memory) = &mut state.memory {
        drop(memory.split(bytes));
    }
}

pub(super) struct Scope<'a> {
    previous: *mut NativeControl,
    _borrow: PhantomData<&'a mut NativeControl>,
}

impl<'a> Scope<'a> {
    pub fn enter(control: &'a mut NativeControl) -> Self {
        static INITIALIZED: Once = Once::new();
        INITIALIZED.call_once(|| {
            // SAFETY: initialization is serialized before every private parser operation.
            unsafe { uqa_sql_xml_initialize() };
        });
        Self {
            // SAFETY: the caller keeps control and its state at stable addresses until this guard and every native allocation have been released.
            previous: unsafe { uqa_sql_xml_enter(control) },
            _borrow: PhantomData,
        }
    }
}

impl Drop for Scope<'_> {
    fn drop(&mut self) {
        // SAFETY: all operation-local native owners are dropped before this scope; the prior scope remains live in the outer call, if any.
        unsafe { uqa_sql_xml_leave(self.previous) };
    }
}

#[cfg(test)]
mod tests;
