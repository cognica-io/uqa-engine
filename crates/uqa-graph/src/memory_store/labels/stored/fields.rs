//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored registry struct fields preserve defaults, duplicate detection and ignored values.

use super::{invalid, Input, JsonToken, StorageBackendResult, Token, Work};

const NAMES: [&str; 6] = [
    "allocation_id",
    "labels",
    "kinds",
    "sequences",
    "dropped_label_ids",
    "next_label_id",
];

pub(super) struct Fields {
    object: bool,
    position: usize,
    seen: [bool; 6],
}

impl Fields {
    pub(super) fn new(first: Token<'_>) -> StorageBackendResult<Self> {
        let object = match first {
            JsonToken::StartObject => true,
            JsonToken::StartArray => false,
            _ => return Err(invalid()),
        };
        Ok(Self {
            object,
            position: 0,
            seen: [false; 6],
        })
    }

    pub(super) async fn next<'a>(
        &mut self,
        source: &mut Input<'a, '_>,
        work: &mut Work<'_, '_>,
    ) -> StorageBackendResult<Option<(usize, Token<'a>)>> {
        loop {
            let token = source.next(work).await?;
            if matches!(
                (self.object, token),
                (true, JsonToken::EndObject) | (false, JsonToken::EndArray)
            ) {
                return Ok(None);
            }
            if !self.object {
                let field = self.position;
                if field == NAMES.len() {
                    return Err(invalid());
                }
                self.position += 1;
                return Ok(Some((field, token)));
            }
            let JsonToken::Key(_) = token else {
                return Err(invalid());
            };
            let name = source.text(token, work).await?;
            let field = NAMES.iter().position(|field| *field == name.as_str());
            drop(name);
            if let Some(field) = field {
                if std::mem::replace(&mut self.seen[field], true) {
                    return Err(invalid());
                }
                return Ok(Some((field, source.next(work).await?)));
            }
            let first = source.next(work).await?;
            skip(source, first, work).await?;
        }
    }
}

async fn skip<'a>(
    source: &mut Input<'a, '_>,
    first: Token<'a>,
    work: &mut Work<'_, '_>,
) -> StorageBackendResult<()> {
    let mut depth = usize::from(matches!(
        first,
        JsonToken::StartArray | JsonToken::StartObject
    ));
    while depth != 0 {
        match source.next(work).await? {
            JsonToken::StartArray | JsonToken::StartObject => depth += 1,
            JsonToken::EndArray | JsonToken::EndObject => depth -= 1,
            _ => (),
        }
    }
    Ok(())
}
