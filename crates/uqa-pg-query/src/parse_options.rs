//! Per-call PostgreSQL scanner settings and structured diagnostics.

use std::ffi::{c_void, CStr, CString};

use prost::Message;

use crate::bindings::*;
use crate::{protobuf, Error, ParseMode, ParseResult, Result};

/// Scanner settings captured when a complete SQL message is parsed. PostgreSQL's
/// safe_encoding and on values for backslash_quote are equivalent for UTF-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseOptions {
    pub mode: ParseMode,
    pub standard_conforming_strings: bool,
    pub backslash_quote: bool,
    pub escape_string_warning: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            mode: ParseMode::Default,
            standard_conforming_strings: true,
            backslash_quote: true,
            escape_string_warning: true,
        }
    }
}

impl ParseOptions {
    pub(crate) fn bits(self) -> i32 {
        self.mode as i32
            | if self.standard_conforming_strings {
                0
            } else {
                PG_QUERY_DISABLE_STANDARD_CONFORMING_STRINGS as i32
            }
            | if self.backslash_quote {
                0
            } else {
                PG_QUERY_DISABLE_BACKSLASH_QUOTE as i32
            }
            | if self.escape_string_warning {
                0
            } else {
                PG_QUERY_DISABLE_ESCAPE_STRING_WARNING as i32
            }
    }
}

/// PostgreSQL ErrorData fields copied while the parser owns their backing storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: i32,
    pub sqlstate: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub context: Option<String>,
    pub cursor_position: i32,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// Notices are retained in parser order, including notices emitted before an error.
#[derive(Debug)]
pub struct ParseOutcome<T> {
    pub result: Result<T>,
    pub diagnostics: Vec<Diagnostic>,
}

impl<T> ParseOutcome<T> {
    pub(crate) fn error(error: Error) -> Self {
        Self {
            result: Err(error),
            diagnostics: Vec::new(),
        }
    }
}

pub(crate) unsafe extern "C" fn capture_diagnostic(
    context: *mut c_void,
    diagnostic: *const PgQueryDiagnostic,
) {
    // This synchronous callback only copies borrowed fields. It never invokes the
    // parser, calls user code, or leaves references into the C memory context.
    let output = &mut *(context as *mut Vec<Diagnostic>);
    let diagnostic = &*diagnostic;
    let text = |value: *const std::os::raw::c_char| {
        if value.is_null() {
            None
        } else {
            Some(CStr::from_ptr(value).to_string_lossy().into_owned())
        }
    };
    output.push(Diagnostic {
        severity: diagnostic.severity,
        sqlstate: CStr::from_ptr(diagnostic.sqlstate.as_ptr())
            .to_string_lossy()
            .into_owned(),
        message: text(diagnostic.message).unwrap_or_default(),
        detail: text(diagnostic.detail),
        hint: text(diagnostic.hint),
        context: text(diagnostic.context),
        cursor_position: diagnostic.cursorpos,
    });
}

pub(crate) unsafe fn parse_error(error: *const PgQueryError, diagnostics: &[Diagnostic]) -> Error {
    if let Some(diagnostic) = diagnostics
        .iter()
        .rev()
        .find(|diagnostic| diagnostic.severity >= 21)
    {
        Error::ParseDiagnostic(diagnostic.clone())
    } else {
        Error::Parse(
            CStr::from_ptr((*error).message)
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// Parse with per-call scanner settings and original SQLSTATE, DETAIL, HINT and
/// warning fields. Settings and callback state are restored even after errors.
pub fn parse_with_options(statement: &str, options: ParseOptions) -> ParseOutcome<ParseResult> {
    let input = match CString::new(statement) {
        Ok(input) => input,
        Err(error) => return ParseOutcome::error(error.into()),
    };
    let mut diagnostics = Vec::<Diagnostic>::new();
    let result = unsafe {
        pg_query_parse_protobuf_with_diagnostics(
            input.as_ptr(),
            options.bits(),
            Some(capture_diagnostic),
            (&mut diagnostics as *mut Vec<Diagnostic>).cast(),
        )
    };
    let parsed = if !result.error.is_null() {
        Err(unsafe { parse_error(result.error, &diagnostics) })
    } else {
        let data = unsafe {
            std::slice::from_raw_parts(result.parse_tree.data as *const u8, result.parse_tree.len)
        };
        protobuf::ParseResult::decode(data)
            .map_err(Error::Decode)
            .map(|tree| ParseResult::new(tree, String::new()))
    };
    unsafe { pg_query_free_protobuf_parse_result(result) };
    ParseOutcome {
        result: parsed,
        diagnostics,
    }
}
