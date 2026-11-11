//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve the shared serde struct shapes without retaining an intermediate value tree.

use super::{decode_json_string, invalid, Input, JsonToken, Result};

impl<'a> Input<'a, '_> {
    pub(super) fn fields<const N: usize>(
        &mut self,
        first: JsonToken<'a>,
        names: [&str; N],
        required: [bool; N],
        mut field: impl FnMut(&mut Self, usize, JsonToken<'a>) -> Result<()>,
    ) -> Result<()> {
        let object = match first {
            JsonToken::StartObject => true,
            JsonToken::StartArray => false,
            _ => return Err(invalid("expected an authority struct")),
        };
        let mut seen = [false; N];
        let mut position = 0;
        loop {
            let token = self.next()?;
            if matches!(
                (object, token),
                (true, JsonToken::EndObject) | (false, JsonToken::EndArray)
            ) {
                break;
            }
            let selected = if object {
                let JsonToken::Key(key) = token else {
                    return Err(invalid("expected an authority field name"));
                };
                let key = decode_json_string(key, self.memory, self.cancellation)?;
                names.iter().position(|name| *name == key.as_str())
            } else {
                if position == N {
                    return Err(invalid("too many positional authority fields"));
                }
                let index = position;
                position += 1;
                Some(index)
            };
            if let Some(index) = selected.filter(|index| seen[*index]) {
                return Err(invalid(format!("duplicate field `{}`", names[index])));
            }
            let value = if object { self.next()? } else { token };
            if let Some(index) = selected {
                seen[index] = true;
                field(self, index, value)?;
            } else {
                self.skip(value)?;
            }
        }
        for (index, name) in names.into_iter().enumerate() {
            if required[index] && !seen[index] {
                return Err(invalid(format!("missing field `{name}`")));
            }
        }
        Ok(())
    }

    fn skip(&mut self, first: JsonToken<'a>) -> Result<()> {
        let mut depth = usize::from(matches!(
            first,
            JsonToken::StartArray | JsonToken::StartObject
        ));
        while depth != 0 {
            match self.next()? {
                JsonToken::StartArray | JsonToken::StartObject => depth += 1,
                JsonToken::EndArray | JsonToken::EndObject => depth -= 1,
                _ => (),
            }
        }
        Ok(())
    }
}
