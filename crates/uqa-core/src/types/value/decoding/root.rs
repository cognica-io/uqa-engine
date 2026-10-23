//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Root visitors borrow ordinary field names and retain only escape scratch.

use super::{decode_json_string, Budgeted, JsonReadError, JsonValueDecoder};

pub(super) enum FieldName<'a> {
    Borrowed(&'a str),
    Decoded(Budgeted<String>),
}

#[cfg(test)]
mod tests;

impl<'a> FieldName<'a> {
    pub(super) fn read(
        token: &'a [u8],
        decoder: &JsonValueDecoder<'_>,
    ) -> Result<Self, JsonReadError> {
        if token.contains(&b'\\') {
            Ok(Self::Decoded(decode_json_string(
                token,
                decoder.memory,
                decoder.cancellation,
            )?))
        } else {
            // JsonReader has already validated quotes, Unicode and controls.
            Ok(Self::Borrowed(
                std::str::from_utf8(&token[1..token.len() - 1])
                    .expect("validated JSON string token"),
            ))
        }
    }

    pub(super) fn as_str(&self) -> &str {
        match self {
            Self::Borrowed(name) => name,
            Self::Decoded(name) => name,
        }
    }
}
