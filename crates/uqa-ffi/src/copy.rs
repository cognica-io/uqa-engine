//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! C stream adapters. Native COPY owns formatting, cancellation and transaction completion.

// Callback addresses and byte spans are supplied by the C contract and used only synchronously.
#![allow(unsafe_code)]

use std::{
    ffi::{c_char, c_void},
    io::{self, Read, Write},
};

use crate::{
    abi::{UQAStatus, UQA_OK},
    boundary,
    callback::Callbacks,
    error::{RetainedError, UQAError},
    session::UQAEngine,
};

pub type UQACopyReadFn = unsafe extern "C" fn(*mut c_void, *mut u8, usize, *mut usize) -> UQAStatus;
pub type UQACopyWriteFn =
    unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut usize) -> UQAStatus;
pub type UQACopyFlushFn = unsafe extern "C" fn(*mut c_void) -> UQAStatus;

#[derive(Debug)]
struct CallbackIoError(RetainedError);

impl std::fmt::Display for CallbackIoError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str(
            self.0
                .get()
                .map_or("COPY callback failed", UQAError::message),
        )
    }
}
impl std::error::Error for CallbackIoError {}

struct Stream<'a> {
    callbacks: Callbacks<'a>,
    user_data: *mut c_void,
}

impl Stream<'_> {
    fn accept(&self, status: UQAStatus) -> io::Result<()> {
        if status == UQA_OK {
            return Ok(());
        }
        if status > 0 {
            self.callbacks.fail(UQAError::invalid(
                "COPY callbacks must return UQA_OK or a negative status",
            ));
        }
        Err(self.error())
    }

    fn invalid(&self, message: &'static str) -> io::Error {
        self.callbacks.fail(UQAError::invalid(message));
        self.error()
    }

    fn error(&self) -> io::Error {
        io::Error::other(CallbackIoError(std::sync::Arc::clone(
            &self.callbacks.failure,
        )))
    }
}

struct Source<'a> {
    stream: Stream<'a>,
    read: UQACopyReadFn,
}

impl Read for Source<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let mut count = 0;
        let status = self.stream.callbacks.invoke(|| unsafe {
            (self.read)(
                self.stream.user_data,
                bytes.as_mut_ptr(),
                bytes.len(),
                &raw mut count,
            )
        });
        self.stream.accept(status)?;
        if count > bytes.len() {
            return Err(self
                .stream
                .invalid("COPY reader returned a count larger than its buffer"));
        }
        Ok(count)
    }
}

struct Sink<'a> {
    stream: Stream<'a>,
    write: UQACopyWriteFn,
    flush: Option<UQACopyFlushFn>,
}

impl Write for Sink<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let mut count = 0;
        let status = self.stream.callbacks.invoke(|| unsafe {
            (self.write)(
                self.stream.user_data,
                bytes.as_ptr(),
                bytes.len(),
                &raw mut count,
            )
        });
        self.stream.accept(status)?;
        if count > bytes.len() {
            return Err(self
                .stream
                .invalid("COPY writer returned a count larger than its input"));
        }
        if count == 0 {
            return Err(self
                .stream
                .invalid("COPY writer made no progress for nonempty input"));
        }
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        let Some(flush) = self.flush else {
            return Ok(());
        };
        let status = self
            .stream
            .callbacks
            .invoke(|| unsafe { flush(self.stream.user_data) });
        self.stream.accept(status)
    }
}

fn copy_from(
    engine: &UQAEngine,
    statement: &str,
    read: Option<UQACopyReadFn>,
    user_data: *mut c_void,
) -> boundary::Result<u64> {
    let read = read.ok_or_else(|| UQAError::invalid("COPY read callback must not be NULL"))?;
    let session = &engine.session;
    let _call = session.enter()?;
    let mut source = Source {
        stream: Stream {
            callbacks: Callbacks::new(session)?,
            user_data,
        },
        read,
    };
    session
        .execute(|native| native.copy_from(statement, &mut source))
        .map_err(|error| error.with_cause(&source.stream.callbacks.failure))
}

fn copy_to(
    engine: &UQAEngine,
    statement: &str,
    write: Option<UQACopyWriteFn>,
    flush: Option<UQACopyFlushFn>,
    user_data: *mut c_void,
) -> boundary::Result<u64> {
    let write = write.ok_or_else(|| UQAError::invalid("COPY write callback must not be NULL"))?;
    let session = &engine.session;
    let _call = session.enter()?;
    let mut sink = Sink {
        stream: Stream {
            callbacks: Callbacks::new(session)?,
            user_data,
        },
        write,
        flush,
    };
    session
        .execute(|native| native.copy_to(statement, &mut sink))
        .map_err(|error| error.with_cause(&sink.stream.callbacks.failure))
}

ffi_fn!(uqa_engine_copy_from(engine: *mut UQAEngine, statement: *const c_char, read: Option<UQACopyReadFn>, user_data: *mut c_void, rows: *mut u64) {
    let rows = boundary::output(rows)?;
    *rows = copy_from(boundary::input(engine)?, boundary::text(statement)?, read, user_data)?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_engine_copy_from_n(engine: *mut UQAEngine, statement: *const c_char, statement_size: usize, read: Option<UQACopyReadFn>, user_data: *mut c_void, rows: *mut u64) {
    let rows = boundary::output(rows)?;
    *rows = copy_from(boundary::input(engine)?, boundary::text_n(statement, statement_size)?, read, user_data)?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_engine_copy_to(engine: *mut UQAEngine, statement: *const c_char, write: Option<UQACopyWriteFn>, flush: Option<UQACopyFlushFn>, user_data: *mut c_void, rows: *mut u64) {
    let rows = boundary::output(rows)?;
    *rows = copy_to(boundary::input(engine)?, boundary::text(statement)?, write, flush, user_data)?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_engine_copy_to_n(engine: *mut UQAEngine, statement: *const c_char, statement_size: usize, write: Option<UQACopyWriteFn>, flush: Option<UQACopyFlushFn>, user_data: *mut c_void, rows: *mut u64) {
    let rows = boundary::output(rows)?;
    *rows = copy_to(boundary::input(engine)?, boundary::text_n(statement, statement_size)?, write, flush, user_data)?;
    Ok(UQA_OK)
});
