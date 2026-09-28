//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Single-result delivery that releases values when a Node environment is closing.

use std::{
    ffi::c_void,
    future::Future,
    marker::PhantomData,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

use napi::{bindgen_prelude::ToNapiValue, sys, Env, JsValue, Result};
use uqa_core::notifications::NotificationFailureKind as Kind;

use super::failure;

pub struct NativePromise<T> {
    value: sys::napi_value,
    marker: PhantomData<T>,
}

impl<T> ToNapiValue for NativePromise<T> {
    unsafe fn to_napi_value(_env: sys::napi_env, value: Self) -> Result<sys::napi_value> {
        Ok(value.value)
    }
}

struct Sender<T> {
    threadsafe: AtomicPtr<c_void>,
    marker: PhantomData<T>,
}

impl<T: ToNapiValue> Sender<T> {
    fn send(self, result: Result<T>) {
        let data = Box::into_raw(Box::new(result)).cast();
        let threadsafe = self
            .threadsafe
            .swap(ptr::null_mut(), Ordering::AcqRel)
            .cast();
        // SAFETY: this sender owns the function's single producer reference.
        // Exactly one result is offered to a queue with capacity one.
        let status = unsafe {
            sys::napi_call_threadsafe_function(
                threadsafe,
                data,
                sys::ThreadsafeFunctionCallMode::nonblocking,
            )
        };
        if status != sys::Status::napi_ok {
            // SAFETY: Node takes ownership only when the call returns napi_ok.
            drop(unsafe { Box::from_raw(data.cast::<Result<T>>()) });
        }
        // napi_closing consumes this producer's reference. Node explicitly
        // forbids any further call, including release, after that result.
        if status != sys::Status::napi_closing {
            // SAFETY: this is the producer's last use of the retained function.
            unsafe {
                sys::napi_release_threadsafe_function(
                    threadsafe,
                    sys::ThreadsafeFunctionReleaseMode::release,
                );
            }
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let threadsafe = self.threadsafe.swap(ptr::null_mut(), Ordering::AcqRel);
        if !threadsafe.is_null() {
            // SAFETY: a cancelled future still owns its producer reference and
            // has never received napi_closing. No subsequent call uses it.
            unsafe {
                sys::napi_release_threadsafe_function(
                    threadsafe.cast(),
                    sys::ThreadsafeFunctionReleaseMode::release,
                );
            }
        }
    }
}

pub(super) fn run<T, F>(env: Env, future: F) -> Result<NativePromise<T>>
where
    T: ToNapiValue + Send + 'static,
    F: Future<Output = Result<T>> + Send + 'static,
{
    let name = env.create_string("UQA notification result")?;
    let mut deferred = ptr::null_mut();
    let mut promise = ptr::null_mut();
    // SAFETY: both output pointers belong to this call on the owning JS thread.
    let status =
        unsafe { sys::napi_create_promise(env.raw(), &raw mut deferred, &raw mut promise) };
    if status != sys::Status::napi_ok {
        return Err(failure(Kind::Capacity));
    }
    let mut threadsafe = ptr::null_mut();
    // SAFETY: deferred is used only by deliver on this environment's JS thread.
    // It is Node-owned; no Rust allocation or Node reference needs finalization.
    let status = unsafe {
        sys::napi_create_threadsafe_function(
            env.raw(),
            ptr::null_mut(),
            ptr::null_mut(),
            name.raw(),
            1,
            1,
            ptr::null_mut(),
            None,
            deferred.cast(),
            Some(deliver::<T>),
            &raw mut threadsafe,
        )
    };
    if status != sys::Status::napi_ok {
        return Err(failure(Kind::Capacity));
    }
    let sender = Sender {
        threadsafe: AtomicPtr::new(threadsafe.cast()),
        marker: PhantomData,
    };
    drop(napi::bindgen_prelude::spawn(async move {
        sender.send(future.await);
    }));
    Ok(NativePromise {
        value: promise,
        marker: PhantomData,
    })
}

unsafe extern "C" fn deliver<T: ToNapiValue>(
    env: sys::napi_env,
    _callback: sys::napi_value,
    context: *mut c_void,
    data: *mut c_void,
) {
    // SAFETY: an accepted result is delivered exactly once, including Node's
    // null-env disposal callback during shutdown.
    let result = unsafe { *Box::from_raw(data.cast::<Result<T>>()) };
    if env.is_null() {
        return;
    }
    // SAFETY: only a live owning env reaches conversion; no JS handles crossed
    // threads inside result. Every value type here owns only native data.
    match result.and_then(|value| unsafe { T::to_napi_value(env, value) }) {
        Ok(value) => {
            // SAFETY: context is the deferred created in this same environment.
            unsafe {
                sys::napi_resolve_deferred(env, context.cast(), value);
            }
        }
        Err(error) => {
            let Ok(length) = isize::try_from(error.reason.len()) else {
                return;
            };
            let mut message = ptr::null_mut();
            let mut value = ptr::null_mut();
            // SAFETY: the string is borrowed only for this call; each following
            // operation is conditional on the previous valid Node handle.
            unsafe {
                if sys::napi_create_string_utf8(
                    env,
                    error.reason.as_ptr().cast(),
                    length,
                    &raw mut message,
                ) == sys::Status::napi_ok
                    && sys::napi_create_error(env, ptr::null_mut(), message, &raw mut value)
                        == sys::Status::napi_ok
                {
                    sys::napi_reject_deferred(env, context.cast(), value);
                }
            }
        }
    }
}
