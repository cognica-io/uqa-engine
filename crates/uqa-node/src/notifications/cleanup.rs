//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A Node-API cleanup hook keeps the environment alive through actual provider release.

use std::{
    ffi::c_void,
    panic::{catch_unwind, AssertUnwindSafe},
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicPtr, Ordering},
        Arc,
    },
};

use napi::{sys, Env, JsValue, Result};
use uqa_core::notifications::NotificationFailureKind as Kind;

use super::{failure, state::State};

// All Node-API operations below occur on the owning JS thread. The execute
// callback touches only State and the atomic failure flag. Opaque Node handles
// are never dereferenced by Rust or used by a Tokio worker.
pub(super) struct Cleanup {
    state: Arc<State>,
    env: AtomicPtr<c_void>,
    work: AtomicPtr<c_void>,
    hook: AtomicPtr<c_void>,
    started: AtomicBool,
    failed: AtomicBool,
}

impl Cleanup {
    pub(super) fn new(env: Env, state: Arc<State>) -> Result<Arc<Self>> {
        let name = env.create_string("UQA notification cleanup")?;
        let cleanup = Arc::new(Self {
            state,
            env: AtomicPtr::new(env.raw().cast()),
            work: AtomicPtr::new(ptr::null_mut()),
            hook: AtomicPtr::new(ptr::null_mut()),
            started: AtomicBool::new(false),
            failed: AtomicBool::new(false),
        });
        let work_data = Arc::into_raw(Arc::clone(&cleanup)).cast_mut().cast();
        let mut work = ptr::null_mut();
        // SAFETY: the work owns one raw Arc until complete; both callbacks use
        // that same allocation, and the resource name belongs to this env.
        let status = unsafe {
            sys::napi_create_async_work(
                env.raw(),
                ptr::null_mut(),
                name.raw(),
                Some(execute),
                Some(complete),
                work_data,
                &raw mut work,
            )
        };
        if status != sys::Status::napi_ok {
            // SAFETY: failed creation cannot call a work callback.
            drop(unsafe { Arc::from_raw(work_data.cast::<Self>()) });
            return Err(failure(Kind::Capacity));
        }
        cleanup.work.store(work.cast(), Ordering::Release);
        let hook_data = Arc::into_raw(Arc::clone(&cleanup)).cast_mut().cast();
        let mut hook = ptr::null_mut();
        // SAFETY: a separate raw Arc belongs to the hook until it is removed.
        let status = unsafe {
            sys::napi_add_async_cleanup_hook(env.raw(), Some(teardown), hook_data, &raw mut hook)
        };
        if status != sys::Status::napi_ok {
            // SAFETY: neither a failed hook nor unqueued work can run callbacks.
            unsafe {
                sys::napi_delete_async_work(env.raw(), work);
                drop(Arc::from_raw(hook_data.cast::<Self>()));
                drop(Arc::from_raw(work_data.cast::<Self>()));
            }
            return Err(failure(Kind::Capacity));
        }
        cleanup.hook.store(hook.cast(), Ordering::Release);
        Ok(cleanup)
    }

    pub(super) fn start(&self) -> Result<()> {
        if self.started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        // SAFETY: called only by the owning JS thread or its cleanup callback;
        // the registered hook retains env and the pre-created work handle.
        let status = unsafe {
            sys::napi_queue_async_work(
                self.env.load(Ordering::Acquire).cast(),
                self.work.load(Ordering::Acquire).cast(),
            )
        };
        if status != sys::Status::napi_ok {
            self.started.store(false, Ordering::Release);
            return Err(failure(Kind::SourceUnavailable));
        }
        Ok(())
    }
}

unsafe extern "C" fn teardown(_handle: sys::napi_async_cleanup_hook_handle, data: *mut c_void) {
    // SAFETY: Node retains the hook's raw Arc until complete removes it.
    let cleanup = unsafe { &*data.cast::<Cleanup>() };
    cleanup.state.stop();
    if cleanup.start().is_err() {
        // JS execution is already disallowed during environment teardown. If
        // native work admission itself fails, finish retained cleanup here so
        // the environment cannot disappear while provider authority survives.
        let env = cleanup.env.load(Ordering::Acquire).cast();
        // SAFETY: queueing failed, so these callbacks cannot run concurrently.
        unsafe {
            execute(env, data);
            complete(env, sys::Status::napi_ok, data);
        }
    }
}

unsafe extern "C" fn execute(_env: sys::napi_env, data: *mut c_void) {
    // SAFETY: the work's raw Arc remains alive through the complete callback.
    let cleanup = unsafe { &*data.cast::<Cleanup>() };
    if catch_unwind(AssertUnwindSafe(|| cleanup.state.cleanup())).is_err() {
        cleanup.failed.store(true, Ordering::Release);
    }
}

unsafe extern "C" fn complete(env: sys::napi_env, status: sys::napi_status, data: *mut c_void) {
    // SAFETY: complete consumes exactly the work-owned Arc, once.
    let cleanup = unsafe { Arc::from_raw(data.cast::<Cleanup>()) };
    let hook = cleanup.hook.swap(ptr::null_mut(), Ordering::AcqRel).cast();
    let work = cleanup.work.swap(ptr::null_mut(), Ordering::AcqRel).cast();
    // SAFETY: completion runs on the original JS thread after execute returns.
    // Removing the hook prevents any further use of its raw Arc.
    let hook_status = unsafe { sys::napi_remove_async_cleanup_hook(hook) };
    if hook_status == sys::Status::napi_ok {
        // SAFETY: this releases the distinct hook-owned strong reference.
        unsafe {
            Arc::decrement_strong_count(data.cast::<Cleanup>());
        }
    }
    // SAFETY: the work callback has finished and this is its completion callback.
    let work_status = unsafe { sys::napi_delete_async_work(env, work) };
    cleanup.state.finish_cleanup(
        cleanup.failed.load(Ordering::Acquire)
            || status != sys::Status::napi_ok
            || hook_status != sys::Status::napi_ok
            || work_status != sys::Status::napi_ok,
    );
}
