//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and retained model readers share their native construction and validation loops.

use super::DictionaryError;
pub(crate) use crate::work::inline;

pub(crate) mod ordering;
pub(crate) mod source;

pub(crate) trait Work: crate::work::Work<DictionaryError> {
    fn utf8_error(
        &self,
        _bytes: &[u8],
        offset: usize,
        error: std::str::Utf8Error,
    ) -> DictionaryError {
        DictionaryError::Utf8Offset { offset, error }
    }
}

impl Work for () {
    fn utf8_error(
        &self,
        bytes: &[u8],
        _offset: usize,
        _error: std::str::Utf8Error,
    ) -> DictionaryError {
        // The ordinary API retains its standard error type, whose offset cannot be constructed.
        // Retained input carries the validated prefix separately and never rescans that prefix.
        std::str::from_utf8(bytes)
            .expect_err("a rejected native UTF-8 fragment belongs to an invalid string")
            .into()
    }
}

impl Work for uqa_core::image_io::AsyncImageInput<'_> {}
