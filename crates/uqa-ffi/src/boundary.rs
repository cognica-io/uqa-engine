//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! C address validation and allocation. Callers still own pointer validity and synchronization.

// These helpers are the audited boundary between C-owned addresses and Rust references.
#![allow(unsafe_code)]

use std::alloc::{alloc, Layout};
use std::ffi::{c_char, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::abi::{UQAStatus, UQA_ERROR_OUT_OF_MEMORY, UQA_ERROR_PANIC, UQA_ERROR_RANGE, UQA_OK};
use crate::error::{self, UQAError};

pub(crate) type Result<T> = std::result::Result<T, UQAError>;

macro_rules! ffi_fn {
    ($name:ident($($arg:ident: $ty:ty),* $(,)?) $body:block) => {
        /// # Safety
        /// Pointers must obey the public C header's validity, lifetime, ownership,
        /// aliasing and synchronization contracts for the entire call.
        #[allow(unsafe_code)]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name($($arg: $ty),*) -> crate::abi::UQAStatus {
            // The public contract supplies address validity; helpers check representable bounds.
            crate::boundary::run(|| unsafe { $body })
        }
    };
}

pub(crate) fn run(operation: impl FnOnce() -> Result<UQAStatus>) -> UQAStatus {
    match catch_unwind(AssertUnwindSafe(|| {
        error::clear();
        let result = operation();
        if matches!(result, Ok(status) if status >= 0) {
            error::clear();
        }
        result
    })) {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => error::publish(error),
        Err(_) => {
            crate::callback::record_panic();
            let mut failure = UQAError::new(UQA_ERROR_PANIC, "panic at the C ABI boundary");
            failure.completion = crate::abi::UQA_COMPLETION_UNKNOWN;
            error::publish(failure)
        }
    }
}

pub(crate) unsafe fn input<'a, T>(value: *const T) -> Result<&'a T> {
    if value.is_null() || !value.is_aligned() {
        return Err(UQAError::invalid("required input is NULL or misaligned"));
    }
    Ok(&*value)
}

pub(crate) unsafe fn input_mut<'a, T>(value: *mut T) -> Result<&'a mut T> {
    if value.is_null() || !value.is_aligned() {
        return Err(UQAError::invalid("required input is NULL or misaligned"));
    }
    Ok(&mut *value)
}

pub(crate) unsafe fn output<'a, T: Default>(value: *mut T) -> Result<&'a mut T> {
    if value.is_null() || !value.is_aligned() {
        return Err(UQAError::invalid("required output is NULL or misaligned"));
    }
    // A C output can be uninitialized; initialize without reading or dropping its old bits.
    value.write(T::default());
    Ok(&mut *value)
}

pub(crate) unsafe fn new_handle<'a, T>(value: *mut *mut T) -> Result<&'a mut *mut T> {
    let out = input_mut(value)?;
    if !out.is_null() {
        return Err(UQAError::invalid("new handle output must contain NULL"));
    }
    Ok(out)
}

pub(crate) unsafe fn release<T>(slot: *mut *mut T) -> Result<UQAStatus> {
    if slot.is_null() {
        return Ok(UQA_OK);
    }
    let slot = input_mut(slot)?;
    let handle = *slot;
    if !handle.is_null() {
        if !handle.is_aligned() {
            return Err(UQAError::invalid("handle is misaligned"));
        }
        *slot = std::ptr::null_mut();
        drop(Box::from_raw(handle));
    }
    Ok(UQA_OK)
}

pub(crate) fn boxed<T>(value: T) -> Result<Box<T>> {
    let layout = Layout::new::<T>();
    if layout.size() == 0 {
        return Ok(Box::new(value));
    }
    // The allocation is checked before initialization; Box receives exactly its own layout.
    unsafe {
        let pointer = alloc(layout).cast::<T>();
        if pointer.is_null() {
            return Err(UQAError::new(
                UQA_ERROR_OUT_OF_MEMORY,
                "handle allocation failed",
            ));
        }
        pointer.write(value);
        Ok(Box::from_raw(pointer))
    }
}

pub(crate) unsafe fn span<'a, T>(data: *const T, count: usize) -> Result<&'a [T]> {
    if count > (isize::MAX as usize) / std::mem::size_of::<T>().max(1) {
        return Err(UQAError::new(
            UQA_ERROR_RANGE,
            "input span exceeds the addressable range",
        ));
    }
    if count == 0 {
        return Ok(&[]);
    }
    if data.is_null() || !data.is_aligned() {
        return Err(UQAError::invalid(
            "nonempty input span is NULL or misaligned",
        ));
    }
    Ok(std::slice::from_raw_parts(data, count))
}

pub(crate) unsafe fn text_n<'a>(data: *const c_char, size: usize) -> Result<&'a str> {
    let text = std::str::from_utf8(span(data.cast::<u8>(), size)?)
        .map_err(|_| UQAError::invalid("text must be valid UTF-8"))?;
    if text.as_bytes().contains(&0) {
        return Err(UQAError::invalid("text contains an embedded NUL"));
    }
    Ok(text)
}

pub(crate) unsafe fn text<'a>(data: *const c_char) -> Result<&'a str> {
    if data.is_null() {
        return Err(UQAError::invalid("C string must not be NULL"));
    }
    let bytes = CStr::from_ptr(data).to_bytes();
    text_n(data, bytes.len())
}

pub(crate) fn copy_text(text: &str) -> Result<String> {
    let mut copy = String::new();
    copy.try_reserve_exact(text.len())
        .map_err(|_| UQAError::new(UQA_ERROR_OUT_OF_MEMORY, "text allocation failed"))?;
    copy.push_str(text);
    Ok(copy)
}
