//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declarations of the C operation functions in `shim/uqa_pg_regex.c` and the values they share with Rust.
#![allow(unsafe_code)] // This module declares the C boundary; callers uphold the documented contracts.

use std::ffi::{c_char, c_int, c_void};

use crate::span::MatchSpan;

/// `PostgreSQL`'s `regex_t`, used only behind pointers returned by `uqa_pg_regex_compile`.
#[repr(C)]
pub(crate) struct RegexT {
    _opaque: [u8; 0],
}

/// `UQAPgRegexThrown`: why an operation left the engine through its C error frame.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Thrown {
    pub(crate) kind: c_int,
    pub(crate) first: usize,
    pub(crate) second: usize,
}

/// Result of an operation that left the engine through its error frame.
pub(crate) const THROWN: c_int = -1000;

pub(crate) const THROW_CANCELED: c_int = 1;
pub(crate) const THROW_CALLBACK: c_int = 2;
pub(crate) const THROW_ALLOC_SIZE: c_int = 3;
pub(crate) const THROW_ALLOC_PRODUCT: c_int = 4;
pub(crate) const THROW_OUT_OF_MEMORY: c_int = 5;
pub(crate) const THROW_LOCALE: c_int = 6;
pub(crate) const THROW_STACK: c_int = 7;

/// Results of the Rust callbacks.
pub(crate) const CALLBACK_OK: c_int = 0;
pub(crate) const CALLBACK_REFUSED: c_int = 1;
pub(crate) const CALLBACK_PANICKED: c_int = 2;
pub(crate) const CALLBACK_RESTART: c_int = 3;

/// `PostgreSQL`'s `errMsg` buffer size in `regexp.c`, including the terminating NUL.
pub(crate) const ERROR_BUFFER_SIZE: usize = 100;

extern "C" {
    /// Compile `length` code points under `locale`; on success `*result` owns a `regex_t` in the operation's memory context.
    pub(crate) fn uqa_pg_regex_compile(
        operation: *mut c_void,
        pattern: *const u32,
        length: usize,
        cflags: c_int,
        locale: u32,
        result: *mut *mut RegexT,
        thrown: *mut Thrown,
    ) -> c_int;

    /// Run `pg_regexec` on `length` code points from `search_start`, filling `nmatch` spans.
    pub(crate) fn uqa_pg_regex_execute(
        operation: *mut c_void,
        expression: *mut RegexT,
        data: *const u32,
        length: usize,
        search_start: usize,
        nmatch: usize,
        pmatch: *mut MatchSpan,
        thrown: *mut Thrown,
    ) -> c_int;

    /// `re_nsub` of a compiled expression.
    pub(crate) fn uqa_pg_regex_subexpressions(expression: *const RegexT) -> usize;

    /// `pg_regerror(code, NULL, buffer, size)`.
    pub(crate) fn uqa_pg_regex_error_message(
        code: c_int,
        buffer: *mut c_char,
        size: usize,
    ) -> usize;

    /// `sizeof(regmatch_t)`, for the layout of [`MatchSpan`].
    #[cfg(test)]
    pub(crate) fn uqa_pg_regex_match_size() -> usize;
}
