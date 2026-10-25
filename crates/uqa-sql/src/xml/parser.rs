//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The only Rust boundary to the private, statically linked XML parser.

use std::ffi::{c_char, c_int, c_void, CStr};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};

mod control;

unsafe extern "C" {
    fn uqa_sql_xml_validate(
        input: *const c_char,
        length: usize,
        document: c_int,
        version: *const c_char,
        version_length: usize,
        detail: *mut *mut c_char,
        notices: *mut *mut c_void,
    ) -> c_int;
    fn uqa_sql_xml_free_detail(detail: *mut c_char);
    fn uqa_sql_xml_name_character(character: u32) -> c_int;
    fn uqa_sql_xml_notice_message(notice: *const c_void, level: *mut c_int) -> *const c_char;
    fn uqa_sql_xml_notice_next(notice: *const c_void) -> *const c_void;
    fn uqa_sql_xml_notices_free(notices: *mut c_void);
}

pub(super) enum Failure {
    Invalid(Option<String>),
    Memory,
    Limit,
    Control(ValueRetentionError),
}

struct Detail(*mut c_char);
struct Notices(*mut c_void);

impl Drop for Notices {
    fn drop(&mut self) {
        // SAFETY: the returned list and its messages belong to this owner and are released together exactly once, including on unwinding from a Rust notice consumer.
        unsafe { uqa_sql_xml_notices_free(self.0) };
    }
}

impl Drop for Detail {
    fn drop(&mut self) {
        // SAFETY: the bridge either leaves NULL or transfers one xml-allocated string, freed exactly once by its paired allocator.
        unsafe { uqa_sql_xml_free_detail(self.0) };
    }
}

pub(super) fn is_name_character(character: char) -> bool {
    // SAFETY: the bridge accepts a Unicode scalar and retains no state.
    unsafe { uqa_sql_xml_name_character(u32::from(character)) != 0 }
}

pub(super) fn validate(
    input: &str,
    document: bool,
    version: &str,
    production: &ProductionControl<'_>,
    mut notice: impl FnMut(crate::SQLNotice),
) -> Result<(), Failure> {
    production.check().map_err(Failure::Control)?;
    if input.len() > c_int::MAX as usize || version.len() > c_int::MAX as usize {
        return Err(Failure::Limit);
    }
    let mut state = control::State::new(*production);
    let mut native = state.native();
    let _scope = control::Scope::enter(&mut native);
    let mut detail = Detail(std::ptr::null_mut());
    let mut notices = Notices(std::ptr::null_mut());
    // SAFETY: both byte slices remain live for the call; their explicit lengths bound every read. The bridge retains neither slice and initializes the detail pointer on every return.
    let status = unsafe {
        uqa_sql_xml_validate(
            input.as_ptr().cast(),
            input.len(),
            c_int::from(document),
            version.as_ptr().cast(),
            version.len(),
            &raw mut detail.0,
            &raw mut notices.0,
        )
    };
    if let Some(error) = state.error.take() {
        return Err(Failure::Control(error));
    }
    let mut current = notices.0.cast_const();
    while !current.is_null() {
        let mut level = 0;
        // SAFETY: current belongs to the live, immutable list returned by the bridge; each message is terminated and remains owned by Notices until the loop finishes.
        let message =
            unsafe { CStr::from_ptr(uqa_sql_xml_notice_message(current, &raw mut level)) };
        let level = if level == 0 {
            crate::NoticeLevel::Notice
        } else {
            crate::NoticeLevel::Warning
        };
        let message = diagnostic_text(message, production)?;
        let (message, _memory) = message.into_parts();
        notice(crate::SQLNotice::new(level, message));
        // SAFETY: the bridge returns the next live list entry or NULL; no callback runs inside C.
        current = unsafe { uqa_sql_xml_notice_next(current) };
    }
    match status {
        0 => Ok(()),
        2 => Err(Failure::Memory),
        _ => {
            let text = if detail.0.is_null() {
                None
            } else {
                // SAFETY: a non-NULL detail is a bridge-owned, terminated string, alive until Detail is dropped.
                let text = diagnostic_text(unsafe { CStr::from_ptr(detail.0) }, production)?;
                let (mut text, _memory) = text.into_parts();
                text.truncate(text.trim_end_matches('\n').len());
                Some(text)
            };
            Err(Failure::Invalid(text))
        }
    }
}

fn diagnostic_text(
    input: &CStr,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, Failure> {
    let mut input = input.to_bytes();
    let mut output = ProductionString::new(*control);
    while !input.is_empty() {
        match std::str::from_utf8(input) {
            Ok(text) => {
                output.push_str(text).map_err(Failure::Control)?;
                break;
            }
            Err(error) => {
                let (prefix, rest) = input.split_at(error.valid_up_to());
                let prefix = std::str::from_utf8(prefix).expect("validated UTF-8 prefix");
                output.push_str(prefix).map_err(Failure::Control)?;
                output.push_str("\u{fffd}").map_err(Failure::Control)?;
                input = &rest[error.error_len().unwrap_or(rest.len())..];
            }
        }
    }
    output.finish().map_err(Failure::Control)
}
