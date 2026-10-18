//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL diagnostic wording for the shared JSON reader's structural failures.

use crate::SQLError;
use uqa_core::json::JsonExpected;

pub(super) fn invalid_input(input: &str, (offset, expected): (usize, JsonExpected)) -> SQLError {
    let bytes = input.as_bytes();
    let detail = if offset == bytes.len() {
        Some("The input string ended unexpectedly.".into())
    } else if bytes[offset].is_ascii_alphanumeric() || bytes[offset] == b'_' || bytes[offset] >= 128
    {
        // A failed keyword may have consumed a prefix; report the complete invalid token.
        let word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 128;
        let mut start = offset;
        while start > 0 && word(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = offset + 1;
        while end < bytes.len() && word(bytes[end]) {
            end += 1;
        }
        let token = &input[start..end];
        if matches!(token, "true" | "false" | "null")
            || serde_json::from_str::<serde::de::IgnoredAny>(token).is_ok()
        {
            Some(unexpected_token(expected, token))
        } else {
            Some(format!("Token \"{token}\" is invalid."))
        }
    } else if matches!(bytes[offset], b'{' | b'}' | b'[' | b']' | b':' | b',') {
        Some(unexpected_token(expected, &input[offset..=offset]))
    } else {
        None
    };
    SQLError::Diagnostic {
        sqlstate: "22P02".into(),
        message: "invalid input syntax for type json".into(),
        detail,
        hint: None,
    }
}

fn unexpected_token(expected: JsonExpected, token: &str) -> String {
    let expected = match expected {
        JsonExpected::Value => "JSON value",
        JsonExpected::End => "end of input",
        JsonExpected::ArrayValueOrEnd => "array element or \"]\"",
        JsonExpected::ArraySeparator => "\",\" or \"]\"",
        JsonExpected::ObjectKeyOrEnd => "string or \"}\"",
        JsonExpected::ObjectSeparator => "\",\" or \"}\"",
        JsonExpected::Key => "string",
        JsonExpected::Colon => "\":\"",
    };
    format!("Expected {expected}, but found \"{token}\".")
}
