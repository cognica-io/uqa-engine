//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    cursor::{Cursor, Raw},
    decode_json_string_from, invalid, AsyncImageInput, JsonToken, Result,
};

pub(super) struct Fields<'a, 'c> {
    cursor: Cursor<'a, 'c>,
    object: bool,
    tag: Option<&'static str>,
    seen: u16,
    index: usize,
}

impl<'a, 'c> Fields<'a, 'c> {
    pub(super) async fn open(
        raw: Raw<'a>,
        tag: Option<&'static str>,
        work: &mut AsyncImageInput<'c>,
    ) -> Result<Self> {
        let mut cursor = Cursor::scalar(raw, work)?;
        let object = match cursor.next(work).await?.token {
            JsonToken::StartObject => true,
            JsonToken::StartArray => false,
            _ => return Err(invalid()),
        };
        cursor.enter()?;
        if !object && tag.is_some() {
            let first = cursor.next(work).await?;
            cursor.value(first, work).await?;
        }
        Ok(Self {
            cursor,
            object,
            tag,
            seen: 0,
            index: 0,
        })
    }

    pub(super) async fn next(
        &mut self,
        names: &[&str],
        deny: bool,
        work: &mut AsyncImageInput<'_>,
    ) -> Result<Option<(usize, Raw<'a>)>> {
        loop {
            work.checkpoint().await?;
            let event = self.cursor.next(work).await?;
            if event.token
                == if self.object {
                    JsonToken::EndObject
                } else {
                    JsonToken::EndArray
                }
            {
                self.cursor.end(work).await?;
                return Ok(None);
            }
            if !self.object {
                if self.index == names.len() {
                    return Err(invalid());
                }
                let value = self.cursor.value(event, work).await?;
                let index = self.index;
                self.index += 1;
                return Ok(Some((index, value)));
            }
            let JsonToken::Key(bytes) = event.token else {
                return Err(invalid());
            };
            let key = decode_json_string_from(bytes, work).await?;
            let selected = if self.tag == Some(key.as_str()) {
                None
            } else if let Some(index) = names.iter().position(|name| *name == key.as_str()) {
                let bit = 1 << index;
                if self.seen & bit != 0 {
                    return Err(invalid());
                }
                self.seen |= bit;
                Some(index)
            } else if deny {
                return Err(invalid());
            } else {
                None
            };
            drop(key);
            let first = self.cursor.next(work).await?;
            let value = self.cursor.value(first, work).await?;
            if let Some(index) = selected {
                return Ok(Some((index, value)));
            }
        }
    }
}

pub(super) async fn tag(
    raw: Raw<'_>,
    name: &'static str,
    variants: &'static [&'static str],
    work: &mut AsyncImageInput<'_>,
) -> Result<&'static str> {
    let mut cursor = Cursor::scalar(raw, work)?;
    let object = match cursor.next(work).await?.token {
        JsonToken::StartObject => true,
        JsonToken::StartArray => false,
        _ => return Err(invalid()),
    };
    cursor.enter()?;
    let mut selected = None;
    loop {
        let event = cursor.next(work).await?;
        if event.token
            == if object {
                JsonToken::EndObject
            } else {
                JsonToken::EndArray
            }
        {
            break;
        }
        let (is_tag, first) = if object {
            let JsonToken::Key(bytes) = event.token else {
                return Err(invalid());
            };
            let key = decode_json_string_from(bytes, work).await?;
            let is_tag = key.as_str() == name;
            drop(key);
            (is_tag, cursor.next(work).await?)
        } else {
            (selected.is_none(), event)
        };
        if is_tag {
            if selected.is_some() {
                return Err(invalid());
            }
            let JsonToken::String(bytes) = first.token else {
                return Err(invalid());
            };
            let value = decode_json_string_from(bytes, work).await?;
            selected = Some(
                variants
                    .iter()
                    .copied()
                    .find(|variant| *variant == value.as_str())
                    .ok_or_else(invalid)?,
            );
        } else {
            cursor
                .value(first, work)
                .await?
                .validate_content(work)
                .await?;
        }
    }
    cursor.end(work).await?;
    selected.ok_or_else(invalid)
}
