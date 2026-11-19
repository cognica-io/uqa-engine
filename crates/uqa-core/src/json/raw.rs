//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Raw values borrow one complete span without changing the enclosing typed grammar.

use super::{JsonReadError, JsonReadWork, JsonReader, JsonSyntaxKind, JsonToken};

impl<'a> JsonReader<'a, '_> {
    /// Locate one original raw value through the same retained grammar and work channel. Escapes are checked without interpreting surrogate pairs, and raw nesting has no typed-value depth limit. The next ordinary event retains the reader's original string and depth policies. The span excludes surrounding whitespace and borrows the source without allocation or reserialization.
    pub async fn read_raw_value_with_work<W: JsonReadWork>(
        &mut self,
        work: &mut W,
    ) -> Result<&'a [u8], W::Error> {
        self.value_span(work, true).await
    }

    /// Borrow a complete value using the reader's ordinary event rules, including strict strings and its configured depth limit. Unlike raw descriptor input, typed numeric and delimiter diagnostics remain unchanged.
    pub async fn read_value_span_with_work<W: JsonReadWork>(
        &mut self,
        work: &mut W,
    ) -> Result<&'a [u8], W::Error> {
        self.value_span(work, false).await
    }

    async fn value_span<W: JsonReadWork>(
        &mut self,
        work: &mut W,
        raw: bool,
    ) -> Result<&'a [u8], W::Error> {
        let first = self
            .next_event_with_policy(work, raw)
            .await?
            .ok_or(JsonReadError::InvalidJson)?;
        if matches!(
            first.token,
            JsonToken::EndArray | JsonToken::EndObject | JsonToken::Key(_)
        ) {
            return Err(self.reject(JsonSyntaxKind::ExpectedValue, false).into());
        }
        let start = first.range.start;
        let mut end = first.range.end;
        let mut depth = usize::from(matches!(
            first.token,
            JsonToken::StartArray | JsonToken::StartObject
        ));
        while depth != 0 {
            let event = self
                .next_event_with_policy(work, raw)
                .await?
                .ok_or(JsonReadError::InvalidJson)?;
            end = event.range.end;
            match event.token {
                JsonToken::StartArray | JsonToken::StartObject => depth += 1,
                JsonToken::EndArray | JsonToken::EndObject => depth -= 1,
                _ => (),
            }
        }
        Ok(&self.input[start..end])
    }
}

#[cfg(test)]
mod tests;
