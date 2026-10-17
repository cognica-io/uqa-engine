//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Errors with `PostgreSQL`'s codes, SQLSTATEs and message texts, produced without heap allocation.

#![allow(unsafe_code)] // Engine explanations are read from `pg_regerror` through the C boundary.

use std::ffi::{c_char, c_int};
use std::fmt;

use crate::ffi::{self, ERROR_BUFFER_SIZE};

/// A message of at most 99 bytes, the text `PostgreSQL`'s `char errMsg[100]` buffer holds.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ErrorMessage {
    bytes: [u8; ERROR_BUFFER_SIZE],
    length: u8,
}

impl ErrorMessage {
    fn formatted(arguments: fmt::Arguments<'_>) -> Self {
        let mut message = Self {
            bytes: [0; ERROR_BUFFER_SIZE],
            length: 0,
        };
        // The writer truncates at the buffer's capacity instead of failing.
        let _ = fmt::write(&mut message, arguments);
        message
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.length)])
            .expect("messages are stored at character boundaries")
    }
}

impl fmt::Write for ErrorMessage {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for character in text.chars() {
            let mut encoded = [0; 4];
            let encoded = character.encode_utf8(&mut encoded).as_bytes();
            let length = usize::from(self.length);
            if length + encoded.len() > ERROR_BUFFER_SIZE - 1 {
                break;
            }
            self.bytes[length..length + encoded.len()].copy_from_slice(encoded);
            self.length = u8::try_from(length + encoded.len())
                .expect("the buffer holds fewer than 256 bytes");
        }
        Ok(())
    }
}

impl fmt::Debug for ErrorMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), formatter)
    }
}

impl fmt::Display for ErrorMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::ops::Deref for ErrorMessage {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<str> for ErrorMessage {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for ErrorMessage {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

/// A failed collation selection, compilation or execution, with the error `PostgreSQL` 18.4 raises in the same situation.
///
/// `PostgreSQL` reports [`RegexError::Code`] from `pg_regcomp` as SQLSTATE `2201B` with `invalid regular expression: <explanation>`, and from `pg_regexec` as `2201B` with `regular expression failed: <explanation>`; see `RE_compile_and_cache` and `RE_wchar_execute` in `src/backend/utils/adt/regexp.c`. The other variants carry their own SQLSTATE and complete message, available through [`RegexError::sqlstate`], [`RegexError::message`] and [`RegexError::hint`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegexError {
    /// The engine returned this `REG_*` code; [`RegexError::message`] is `pg_regerror`'s explanation of it.
    Code(i32),
    /// [`crate::RegexResources::cancel_requested`] stopped the operation. `PostgreSQL` 18 has no `REG_CANCEL` code: its engine checks interrupts with `CHECK_FOR_INTERRUPTS()`, which raises the canceled statement's own error, so the caller reports its cancellation error.
    Cancelled,
    /// The parser could not resolve the input collation (`42P22`).
    IndeterminateCollation,
    /// A nondeterministic input collation (`0A000`).
    NondeterministicCollation,
    /// A collation OID with no catalog entry (`XX000`).
    UnknownCollation(u32),
    /// An allocation request above 1 GB minus one byte (`XX000`).
    InvalidAllocationSize(usize),
    /// An allocation request whose element count times element size overflows (`54000`).
    InvalidAllocationProduct(usize, usize),
    /// An allocation that may not fail did fail (`53200`). The engine itself allocates only with `MCXT_ALLOC_NO_OOM` and reports [`crate::REG_ESPACE`] instead.
    OutOfMemory,
    /// An internal locale token the C shim does not know (`XX000`); the safe API cannot produce one.
    UnknownLocale(u32),
}

impl RegexError {
    /// The `REG_*` code of an engine error.
    pub fn code(&self) -> Option<i32> {
        match self {
            Self::Code(code) => Some(*code),
            _ => None,
        }
    }

    /// The SQLSTATE `PostgreSQL` reports; `None` for [`RegexError::Cancelled`], whose error belongs to the caller.
    pub fn sqlstate(&self) -> Option<&'static str> {
        Some(match self {
            Self::Code(_) => "2201B",
            Self::Cancelled => return None,
            Self::IndeterminateCollation => "42P22",
            Self::NondeterministicCollation => "0A000",
            Self::OutOfMemory => "53200",
            Self::InvalidAllocationProduct(..) => "54000",
            Self::UnknownCollation(_) | Self::InvalidAllocationSize(_) | Self::UnknownLocale(_) => {
                "XX000"
            }
        })
    }

    /// The message text: `pg_regerror`'s explanation for an engine error, the complete message otherwise, and empty for [`RegexError::Cancelled`].
    pub fn message(&self) -> ErrorMessage {
        match *self {
            Self::Code(code) => explanation(code),
            Self::Cancelled => ErrorMessage::formatted(format_args!("")),
            Self::IndeterminateCollation => ErrorMessage::formatted(format_args!(
                "could not determine which collation to use for regular expression"
            )),
            Self::NondeterministicCollation => ErrorMessage::formatted(format_args!(
                "nondeterministic collations are not supported for regular expressions"
            )),
            Self::UnknownCollation(oid) => {
                ErrorMessage::formatted(format_args!("cache lookup failed for collation {oid}"))
            }
            Self::InvalidAllocationSize(size) => {
                ErrorMessage::formatted(format_args!("invalid memory alloc request size {size}"))
            }
            Self::InvalidAllocationProduct(first, second) => ErrorMessage::formatted(format_args!(
                "invalid memory allocation request size {first} * {second}"
            )),
            Self::OutOfMemory => ErrorMessage::formatted(format_args!("out of memory")),
            Self::UnknownLocale(token) => ErrorMessage::formatted(format_args!(
                "unrecognized regular expression locale {token}"
            )),
        }
    }

    /// The hint `PostgreSQL` attaches, if any.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::IndeterminateCollation => {
                Some("Use the COLLATE clause to set the collation explicitly.")
            }
            _ => None,
        }
    }
}

/// `pg_regerror`'s explanation of `code`, written into a 100-byte buffer as `regexp.c` does.
fn explanation(code: i32) -> ErrorMessage {
    let mut buffer = [0u8; ERROR_BUFFER_SIZE];
    // SAFETY: pg_regerror writes at most `size` bytes, NUL included. It reads the buffer as a C string only for the REG_ATOI and REG_ITOA conversions, and the zeroed buffer is a valid empty string then.
    unsafe {
        ffi::uqa_pg_regex_error_message(
            c_int::from(code),
            buffer.as_mut_ptr().cast::<c_char>(),
            buffer.len(),
        );
    }
    let length = buffer
        .iter()
        .position(|byte| *byte == 0)
        .expect("pg_regerror terminates its explanation");
    let text =
        std::str::from_utf8(&buffer[..length]).expect("pg_regerror explanations are ASCII text");
    ErrorMessage::formatted(format_args!("{text}"))
}

impl fmt::Display for RegexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("regular expression operation canceled"),
            _ => formatter.write_str(self.message().as_str()),
        }
    }
}

impl std::error::Error for RegexError {}
