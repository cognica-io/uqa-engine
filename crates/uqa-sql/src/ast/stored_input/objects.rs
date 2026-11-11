//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken};

impl<'a> Input<'a, '_> {
    pub fn object(
        &mut self,
        first: JsonToken<'a>,
        mut field: impl FnMut(&mut Self, &str, JsonToken<'a>) -> Result<(), InputError>,
    ) -> Result<(), InputError> {
        if first != JsonToken::StartObject {
            return Err(invalid());
        }
        self.enter()?;
        loop {
            let token = self.next()?;
            if token == JsonToken::EndObject {
                break;
            }
            if !matches!(token, JsonToken::Key(_)) {
                return Err(invalid());
            }
            let key = self.text(token)?;
            let token = self.next()?;
            field(self, &key, token)?;
        }
        self.remaining_depth += 1;
        Ok(())
    }

    /// Flattened Serde fields first validate generic content, then bind selected fields. Preserve that order without retaining a second owned JSON tree.
    pub fn content(&mut self, first: JsonToken<'a>) -> Result<&'a str, InputError> {
        let start = self.range.start;
        let mut token = first;
        let mut depth = 0;
        loop {
            match token {
                JsonToken::StartArray | JsonToken::StartObject => {
                    self.enter()?;
                    depth += 1;
                }
                JsonToken::EndArray | JsonToken::EndObject => {
                    depth -= 1;
                    self.remaining_depth += 1;
                }
                JsonToken::String(_) | JsonToken::Key(_) => {
                    // Generic content validates escapes even for fields that the flattened enum does not select.
                    self.text(token)?;
                }
                JsonToken::Null | JsonToken::Bool(_) | JsonToken::Number(_) => (),
            }
            if depth == 0 {
                break;
            }
            token = self.next()?;
        }
        Ok(&self.text[start..self.range.end])
    }
}
