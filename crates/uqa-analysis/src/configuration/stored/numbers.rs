//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed atoms and generic tagged content retain their distinct public Serde representations.

use super::{
    cursor::{Cursor, Raw},
    decode_json_string_from, invalid, AsyncImageInput, Budgeted, JsonToken, Result,
};
use serde::{
    de::{
        value::{
            BoolDeserializer, I64Deserializer, MapAccessDeserializer, MapDeserializer,
            StrDeserializer, U64Deserializer,
        },
        DeserializeOwned,
    },
    Deserializer,
};
use uqa_core::memory::{BudgetedString, MemoryError};

pub(super) fn is_map(text: &str) -> bool {
    if text.len() > 20 {
        return true;
    }
    if text.starts_with('-') {
        text.parse::<i64>().is_err()
    } else {
        text.parse::<u64>().is_err()
    }
}

pub(super) async fn atom<T: DeserializeOwned>(
    raw: Raw<'_>,
    work: &mut AsyncImageInput<'_>,
) -> Result<T> {
    let mut cursor = Cursor::scalar(raw, work)?;
    let value = match cursor.next(work).await?.token {
        JsonToken::Bool(value) => T::deserialize(BoolDeserializer::<serde_json::Error>::new(value)),
        JsonToken::Number(bytes) if bytes.len() <= 20 => {
            let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
            if text.starts_with('-') {
                T::deserialize(I64Deserializer::<serde_json::Error>::new(
                    text.parse().map_err(|_| invalid())?,
                ))
            } else {
                T::deserialize(U64Deserializer::<serde_json::Error>::new(
                    text.parse().map_err(|_| invalid())?,
                ))
            }
        }
        JsonToken::String(bytes) => {
            let text = decode_json_string_from(bytes, work).await?;
            T::deserialize(StrDeserializer::<serde_json::Error>::new(&text))
        }
        JsonToken::StartObject => {
            let JsonToken::Key(bytes) = cursor.next(work).await?.token else {
                return Err(invalid());
            };
            let key = decode_json_string_from(bytes, work).await?;
            if cursor.next(work).await?.token != JsonToken::Null
                || cursor.next(work).await?.token != JsonToken::EndObject
            {
                return Err(invalid());
            }
            T::deserialize(MapAccessDeserializer::new(MapDeserializer::<
                _,
                serde_json::Error,
            >::new(
                [(key.as_str(), ())].into_iter(),
            )))
        }
        _ => return Err(invalid()),
    }
    .map_err(|_| invalid())?;
    cursor.end(work).await?;
    Ok(value)
}

/// Observe the generic number's map key through the public visitor protocol, without naming dependency-private fields.
pub(super) async fn key(work: &mut AsyncImageInput<'_>) -> Result<Budgeted<String>> {
    struct Key;
    impl<'de> serde::de::Visitor<'de> for Key {
        type Value = &'de str;
        fn expecting(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            out.write_str("a generic number")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let key = map
                .next_key::<&str>()?
                .ok_or_else(|| serde::de::Error::custom("missing numeric key"))?;
            map.next_value::<serde::de::IgnoredAny>()?;
            Ok(key)
        }
    }
    work.checkpoint().await?;
    // The fixed three-byte scalar probe has a fixed serializer scratch allowance.
    let scratch = work.memory.reserve(128)?;
    let key = serde_json::Deserializer::from_str("0.5")
        .deserialize_any(Key)
        .map_err(|_| invalid())?;
    drop(scratch);
    Ok(work.copy_text(key).await?)
}

/// Generic raw numeric content preserves mantissa digits and normalizes only exponent spelling.
pub(super) async fn text(value: &str, work: &mut AsyncImageInput<'_>) -> Result<Budgeted<String>> {
    let mut exponent = None;
    for (chunk, bytes) in value.as_bytes().chunks(4096).enumerate() {
        work.checkpoint().await?;
        if let Some(index) = bytes.iter().position(|byte| matches!(byte, b'e' | b'E')) {
            exponent = Some(chunk * 4096 + index);
            break;
        }
    }
    let mut output = BudgetedString::new(work.memory);
    output.reserve(
        value
            .len()
            .checked_add(1)
            .ok_or(MemoryError::SizeOverflow)?,
    )?;
    let mantissa = exponent.map_or(value, |index| &value[..index]);
    for bytes in mantissa.as_bytes().chunks(4096) {
        work.checkpoint().await?;
        output.push_str(std::str::from_utf8(bytes).map_err(|_| invalid())?)?;
    }
    if let Some(index) = exponent {
        output.push('e')?;
        let tail = &value[index + 1..];
        if !tail.starts_with(['+', '-']) {
            output.push('+')?;
        }
        for bytes in tail.as_bytes().chunks(4096) {
            work.checkpoint().await?;
            output.push_str(std::str::from_utf8(bytes).map_err(|_| invalid())?)?;
        }
    }
    let (value, memory) = output.into_parts();
    Ok(Budgeted::new(value, memory))
}
