//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! C session admission and retained-resource leases; SQL and storage remain native owners.

use std::ffi::c_char;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, OnceLock,
};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_engine::Engine;

use crate::{
    abi::{
        UQA_CAP_INDEPENDENT_SESSIONS, UQA_CAP_PERSISTENCE, UQA_COMPLETION_IN_TRANSACTION,
        UQA_COMPLETION_PENDING, UQA_COMPLETION_UNKNOWN, UQA_ERROR_BUSY, UQA_ERROR_IO,
        UQA_ERROR_UNSUPPORTED, UQA_OK, UQA_TRANSACTION_ACTIVE, UQA_TRANSACTION_COMPLETION_PENDING,
        UQA_TRANSACTION_FAILED, UQA_TRANSACTION_IDLE,
    },
    boundary, callback,
    config::UQAConfig,
    error::{RetainedError, UQAError},
    library,
};

pub struct UQAEngine {
    pub(crate) session: Arc<Session>,
}

pub(crate) struct Session {
    pub(crate) native: Engine,
    pub(crate) memory: MemoryBudget,
    pub(crate) cancellation: CancellationToken,
    pub(crate) sql_notification_error: OnceLock<RetainedError>,
    persistent: bool,
    busy: AtomicBool,
    poisoned: AtomicBool,
}

impl Session {
    fn new(native: Engine, persistent: bool, memory_limit: usize) -> Self {
        let cancellation = native.cancellation_token();
        Self {
            native,
            memory: MemoryBudget::new(memory_limit),
            cancellation,
            sql_notification_error: OnceLock::new(),
            persistent,
            busy: AtomicBool::new(false),
            poisoned: AtomicBool::new(false),
        }
    }

    pub(crate) fn enter(&self) -> boundary::Result<SessionCall<'_>> {
        self.enter_inner(false)
    }

    pub(crate) fn enter_cleanup(&self) -> boundary::Result<SessionCall<'_>> {
        self.enter_inner(true)
    }

    pub(crate) fn enter_value(&self) -> boundary::Result<SessionCall<'_>> {
        if callback::uses_value_scope(self)? {
            self.check_poison()?;
            Ok(SessionCall {
                session: self,
                release: false,
            })
        } else {
            self.enter()
        }
    }

    pub(crate) fn check_poison(&self) -> boundary::Result<()> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(UQAError::state("session is poisoned after a panic"));
        }
        Ok(())
    }

    pub(crate) fn poison(&self) {
        self.poisoned.store(true, Ordering::Release);
    }

    fn enter_inner(&self, cleanup: bool) -> boundary::Result<SessionCall<'_>> {
        callback::reject_reentry()?;
        if self
            .busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(UQAError::new(
                UQA_ERROR_BUSY,
                "session already has an active call",
            ));
        }
        let call = SessionCall {
            session: self,
            release: true,
        };
        if !cleanup {
            self.check_poison()?;
        }
        Ok(call)
    }

    pub(crate) fn control(&self) -> ProductionControl<'_> {
        ProductionControl::new(&self.memory, &self.cancellation, &self.cancellation)
    }

    pub(crate) fn state(&self) -> u32 {
        if self.native.pending_transaction_completion().is_some() {
            UQA_TRANSACTION_COMPLETION_PENDING
        } else if self.native.transaction_failed() {
            UQA_TRANSACTION_FAILED
        } else if self.native.transaction_depth() != 0 {
            UQA_TRANSACTION_ACTIVE
        } else {
            UQA_TRANSACTION_IDLE
        }
    }

    pub(crate) fn execution_error(&self, error: impl Into<UQAError>) -> UQAError {
        let mut error = error.into();
        error.completion = match self.state() {
            UQA_TRANSACTION_COMPLETION_PENDING => UQA_COMPLETION_PENDING,
            UQA_TRANSACTION_ACTIVE | UQA_TRANSACTION_FAILED => UQA_COMPLETION_IN_TRANSACTION,
            // An idle session alone does not establish which SQL segments committed.
            _ => UQA_COMPLETION_UNKNOWN,
        };
        error
    }
}

pub(crate) struct SessionCall<'a> {
    session: &'a Session,
    release: bool,
}

impl Drop for SessionCall<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.session.poison();
        }
        if self.release {
            self.session.busy.store(false, Ordering::Release);
        }
    }
}

fn publish_engine(
    native: Engine,
    persistent: bool,
    limit: usize,
) -> boundary::Result<*mut UQAEngine> {
    let session = Arc::new(Session::new(native, persistent, limit));
    // On a recoverable handle-allocation failure, clean up the native session before returning.
    let handle = boundary::boxed(UQAEngine {
        session: Arc::clone(&session),
    });
    match handle {
        Ok(handle) => Ok(Box::into_raw(handle)),
        Err(error) => {
            session
                .native
                .close()
                .map_err(|cleanup| session.execution_error(cleanup))?;
            Err(error)
        }
    }
}

ffi_fn!(uqa_engine_open_memory(config: *mut UQAConfig, out: *mut *mut UQAEngine) {
    let out = boundary::new_handle(out)?;
    callback::reject_reentry()?;
    let defaults = UQAConfig::default();
    let config = if config.is_null() { &defaults } else { boundary::input(config)? };
    if config.key.is_some() || config.compression.is_some() {
        return Err(UQAError::invalid("memory databases do not accept encryption or compression"));
    }
    *out = publish_engine(Engine::new(), false, config.memory_limit)?;
    Ok(UQA_OK)
});

fn open(path: &str, config: Option<&UQAConfig>, auto: bool) -> boundary::Result<*mut UQAEngine> {
    callback::reject_reentry()?;
    if path.is_empty() {
        return Err(UQAError::invalid("database path must not be empty"));
    }
    let defaults = UQAConfig::default();
    let config = config.unwrap_or(&defaults);
    let key = config.key.as_ref().map(|key| key.as_str());
    let path = Path::new(path);
    let native = if auto {
        if config.compression.is_some() {
            return Err(UQAError::invalid(
                "automatic open detects compression from the database file",
            ));
        }
        Engine::open_auto(path, key)
    } else {
        match (key, config.compression) {
            (None, None) => Engine::open(path),
            (Some(key), None) => Engine::open_encrypted(path, key),
            (None, Some(options)) => Engine::open_compressed(path, options),
            (Some(key), Some(options)) => Engine::open_compressed_encrypted(path, key, options),
        }
    }
    .map_err(|error| UQAError::display(UQA_ERROR_IO, &error))?;
    publish_engine(native, true, config.memory_limit)
}

ffi_fn!(uqa_engine_open(path: *const c_char, config: *mut UQAConfig, out: *mut *mut UQAEngine) {
    let out = boundary::new_handle(out)?;
    let path = boundary::text(path)?;
    let config = if config.is_null() { None } else { Some(boundary::input(config)?) };
    *out = open(path, config, false)?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_engine_open_auto(path: *const c_char, config: *mut UQAConfig, out: *mut *mut UQAEngine) {
    let out = boundary::new_handle(out)?;
    let path = boundary::text(path)?;
    let config = if config.is_null() { None } else { Some(boundary::input(config)?) };
    *out = open(path, config, true)?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_engine_open_session(source: *mut UQAEngine, out: *mut *mut UQAEngine) {
    let out = boundary::new_handle(out)?;
    let session = &boundary::input(source)?.session;
    let _call = session.enter()?;
    if !session.persistent { return Err(UQAError::new(UQA_ERROR_UNSUPPORTED, "memory databases do not support independent sessions")); }
    let native = session.native.new_session().map_err(|error| UQAError::display(UQA_ERROR_IO, &error))?;
    *out = publish_engine(native, true, session.memory.limit())?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_engine_open_session_for_user(source: *mut UQAEngine, role: *const c_char, out: *mut *mut UQAEngine) {
    let out = boundary::new_handle(out)?;
    let role = boundary::text(role)?;
    let session = &boundary::input(source)?.session;
    let _call = session.enter()?;
    if !session.persistent { return Err(UQAError::new(UQA_ERROR_UNSUPPORTED, "memory databases do not support independent sessions")); }
    let native = session.native.new_session_for_user(role)?;
    *out = publish_engine(native, true, session.memory.limit())?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_engine_get_capabilities(engine: *mut UQAEngine, out: *mut u64) {
    let out = boundary::output(out)?;
    let session = &boundary::input(engine)?.session;
    let _call = session.enter_inner(true)?;
    *out = library::capabilities() | if session.persistent { UQA_CAP_PERSISTENCE | UQA_CAP_INDEPENDENT_SESSIONS } else { 0 };
    Ok(UQA_OK)
});
ffi_fn!(uqa_engine_close(engine: *mut *mut UQAEngine) {
    if engine.is_null() { return Ok(UQA_OK); }
    let pointer = *boundary::input(engine)?;
    if pointer.is_null() { return Ok(UQA_OK); }
    {
        let session = &boundary::input(pointer)?.session;
        let _call = session.enter_inner(true)?;
        if Arc::strong_count(session) != 1 {
            return Err(UQAError::new(UQA_ERROR_BUSY, "session still has retained resources"));
        }
        session.native.close().map_err(|error| session.execution_error(error))?;
    }
    boundary::release(engine)
});

macro_rules! transaction_operation {
    ($name:ident, $operation:ident) => {
        ffi_fn!($name(engine: *mut UQAEngine) {
            let session = &boundary::input(engine)?.session;
            let _call = session.enter()?;
            session.native.$operation().map_err(|error| session.execution_error(error))?;
            Ok(UQA_OK)
        });
    };
}
transaction_operation!(uqa_transaction_begin, begin);
transaction_operation!(uqa_transaction_commit, commit);
transaction_operation!(uqa_transaction_rollback, rollback);

macro_rules! savepoint_operation {
    ($name:ident, $operation:ident) => {
        ffi_fn!($name(engine: *mut UQAEngine, name: *const c_char) {
            let name = boundary::text(name)?;
            let session = &boundary::input(engine)?.session;
            let _call = session.enter()?;
            session.native.$operation(name).map_err(|error| session.execution_error(error))?;
            Ok(UQA_OK)
        });
    };
}
savepoint_operation!(uqa_savepoint_create, savepoint);
savepoint_operation!(uqa_savepoint_release, release_savepoint);
savepoint_operation!(uqa_savepoint_rollback, rollback_to_savepoint);

ffi_fn!(uqa_transaction_get_state(engine: *mut UQAEngine, out: *mut u32) {
    let out = boundary::output(out)?;
    let session = &boundary::input(engine)?.session;
    let _call = session.enter_inner(true)?;
    *out = session.state();
    Ok(UQA_OK)
});

pub struct UQACancelHandle(CancellationToken);
ffi_fn!(uqa_cancel_handle_create(engine: *mut UQAEngine, out: *mut *mut UQACancelHandle) {
    let out = boundary::new_handle(out)?;
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    *out = Box::into_raw(boundary::boxed(UQACancelHandle(session.cancellation.clone()))?);
    Ok(UQA_OK)
});
ffi_fn!(uqa_cancel_handle_request(handle: *mut UQACancelHandle) {
    boundary::input(handle)?.0.cancel();
    Ok(UQA_OK)
});
ffi_fn!(uqa_cancel_handle_destroy(handle: *mut *mut UQACancelHandle) { boundary::release(handle) });
ffi_fn!(uqa_engine_reset_cancellation(engine: *mut UQAEngine) {
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    session.native.reset_cancellation();
    Ok(UQA_OK)
});
ffi_fn!(uqa_engine_check_termination(engine: *mut UQAEngine) {
    let session = &boundary::input(engine)?.session;
    let _call = session.enter_inner(true)?;
    match session.native.session_termination() {
        Some(error) => Err(error.into()),
        None => Ok(UQA_OK),
    }
});
