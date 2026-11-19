//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, AsyncImageInput, JsonToken, Result};
use uqa_core::json::{JsonEvent, JsonReader};

#[derive(Clone, Copy)]
pub(super) enum Raw<'a> {
    Json {
        text: &'a str,
        content: bool,
        depth: usize,
    },
    NumberText(&'a str),
}

impl<'a> Raw<'a> {
    pub(super) fn json(text: &'a str, content: bool, depth: usize) -> Self {
        Self::Json {
            text,
            content,
            depth,
        }
    }
    pub(super) fn content(self) -> Self {
        match self {
            Self::Json { text, depth, .. } => Self::json(text, true, depth),
            value @ Self::NumberText(_) => value,
        }
    }
    pub(super) async fn validate_content(self, work: &mut AsyncImageInput<'_>) -> Result<()> {
        let Self::Json { text, depth, .. } = self else {
            return Ok(());
        };
        let mut reader = JsonReader::new(text, work.memory, work.cancellation)
            .with_depth_limit(depth.saturating_sub(1));
        while reader.next_event_from(work).await?.is_some() {}
        Ok(())
    }
}

pub(super) struct Cursor<'a, 'c> {
    reader: JsonReader<'a, 'c>,
    text: &'a str,
    content: bool,
    depth: usize,
}

impl<'a, 'c> Cursor<'a, 'c> {
    pub(super) fn scalar(raw: Raw<'a>, work: &AsyncImageInput<'c>) -> Result<Self> {
        let Raw::Json {
            text,
            content,
            depth,
        } = raw
        else {
            return Err(invalid());
        };
        Ok(Self {
            reader: JsonReader::new(text, work.memory, work.cancellation)
                .with_ignored_string_escapes(),
            text,
            content,
            depth,
        })
    }

    pub(super) async fn container(
        raw: Raw<'a>,
        work: &mut AsyncImageInput<'c>,
        expected: JsonToken<'_, &[u8]>,
    ) -> Result<Self> {
        let mut cursor = Self::scalar(raw, work)?;
        if cursor.next(work).await?.token != expected {
            return Err(invalid());
        }
        cursor.enter()?;
        Ok(cursor)
    }

    pub(super) fn enter(&mut self) -> Result<()> {
        self.depth = self
            .depth
            .checked_sub(1)
            .filter(|depth| *depth != 0)
            .ok_or_else(invalid)?;
        Ok(())
    }

    pub(super) async fn next(
        &mut self,
        work: &mut AsyncImageInput<'_>,
    ) -> Result<JsonEvent<'a, &'a [u8]>> {
        self.reader.next_event_from(work).await?.ok_or_else(invalid)
    }

    pub(super) async fn end(&mut self, work: &mut AsyncImageInput<'_>) -> Result<()> {
        if self.reader.next_event_from(work).await?.is_some() {
            return Err(invalid());
        }
        Ok(())
    }

    pub(super) async fn value(
        &mut self,
        first: JsonEvent<'a, &'a [u8]>,
        work: &mut AsyncImageInput<'_>,
    ) -> Result<Raw<'a>> {
        let start = first.range.start;
        let mut end = first.range.end;
        let mut depth = usize::from(matches!(
            first.token,
            JsonToken::StartArray | JsonToken::StartObject
        ));
        while depth != 0 {
            let event = self.next(work).await?;
            end = event.range.end;
            match event.token {
                JsonToken::StartArray | JsonToken::StartObject => depth += 1,
                JsonToken::EndArray | JsonToken::EndObject => depth -= 1,
                _ => (),
            }
        }
        Ok(Raw::json(&self.text[start..end], self.content, self.depth))
    }
}
