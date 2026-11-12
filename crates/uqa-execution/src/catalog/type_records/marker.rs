//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Format markers share original generic value semantics without constructing discarded value trees.

use super::TypeRecordFormat;
use uqa_core::{
    image_io::AsyncImageInput,
    json::{decode_json_string_with_work, JsonReadError, JsonReadWork, JsonReader, JsonToken},
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

mod protocol;
#[cfg(test)]
mod tests;
mod value;

type Token<'a> = JsonToken<'a, &'a [u8]>;

pub(super) fn check(format: &TypeRecordFormat, text: &str) -> StorageBackendResult<()> {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let memory = MemoryBudget::new(usize::MAX);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut work = ();
    let mut future = std::pin::pin!(check_with_work(format, text, &control, &mut work));
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(result) => result,
        Poll::Pending => unreachable!("inline marker work is immediately ready"),
    }
}

pub(super) async fn check_from(
    format: &TypeRecordFormat,
    text: &str,
    control: &ProductionControl<'_>,
    work: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    check_with_work(format, text, control, work).await
}

async fn check_with_work<W: JsonReadWork<Error = JsonReadError>>(
    format: &TypeRecordFormat,
    text: &str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> StorageBackendResult<()> {
    let keys = protocol::keys(control)?;
    let mut reader = JsonReader::with_control(text, control).with_depth_limit(127);
    if next(&mut reader, work).await? != JsonToken::StartObject {
        return Err(invalid());
    }
    let mut seen = false;
    let mut foreign = false;
    let mut version = None;
    loop {
        let token = next(&mut reader, work).await?;
        if token == JsonToken::EndObject {
            break;
        }
        let JsonToken::Key(encoded) = token else {
            return Err(invalid());
        };
        let name = decode_json_string_with_work(encoded, control, work)
            .await
            .map_err(json_error)?;
        let matches = name.as_str() == format.format_field;
        drop(name);
        let token = next(&mut reader, work).await?;
        let scalar = value::read(token, &mut reader, control, work, keys).await?;
        if matches {
            seen = true;
            version = scalar;
        } else {
            foreign = true;
        }
    }
    if reader
        .next_event_with_work(work)
        .await
        .map_err(json_error)?
        .is_some()
    {
        return Err(invalid());
    }
    control.check()?;
    let version = version.filter(|_| seen && !foreign).ok_or_else(|| {
        TypeRecordFormat::corrupt(format!("malformed {} catalog format marker", format.label))
    })?;
    if version != format.version {
        return Err(TypeRecordFormat::corrupt(format!(
            "unsupported {} catalog format {version}",
            format.label
        )));
    }
    Ok(())
}

async fn next<'a, W: JsonReadWork<Error = JsonReadError>>(
    reader: &mut JsonReader<'a, '_>,
    work: &mut W,
) -> StorageBackendResult<Token<'a>> {
    reader
        .next_event_with_work(work)
        .await
        .map_err(json_error)?
        .map(|event| event.token)
        .ok_or_else(invalid)
}

fn invalid() -> StorageBackendError {
    StorageBackendError::Serde(<serde_json::Error as serde::de::Error>::custom(
        "invalid type catalog format JSON",
    ))
}
fn json_error(error: JsonReadError) -> StorageBackendError {
    match error {
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
        _ => invalid(),
    }
}
