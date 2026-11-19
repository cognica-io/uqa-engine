//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical metadata keys admit decoded identities without allocating their serialized spelling.

use super::{RelationIdentity, METADATA_PREFIX};
use crate::{
    catalog::input::strings, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};
use std::io::Write;
use uqa_core::{
    json::{decode_json_string, JsonReader, JsonToken},
    memory::Budgeted,
};

/// Decode the original relation/column key and retain each original admitted string. Canonical relation spelling is checked before the column suffix, matching ordinary catalog restoration.
pub fn decode_key_with_control(
    key: &str,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<(RelationIdentity, Option<String>)>> {
    control.check()?;
    let suffix = key
        .strip_prefix(METADATA_PREFIX)
        .ok_or_else(|| invalid("invalid ACL metadata prefix"))?;
    let ([schema, name], end) = strings::fields_prefix(
        suffix.as_bytes(),
        [("schema", false), ("name", false)],
        control,
    )?;
    let (schema, mut memory) = schema.expect("required schema").into_parts();
    let (name, name_memory) = name.expect("required name").into_parts();
    memory.absorb(name_memory);
    let relation = Budgeted::new(RelationIdentity { schema, name }, memory);
    if suffix.as_bytes().get(end) != Some(&b':')
        || !canonical(&suffix.as_bytes()[..end], &*relation, control)?
    {
        return Err(invalid("ACL tuple belongs to another relation"));
    }
    let suffix = &suffix.as_bytes()[end + 1..];
    let column = column(suffix, control)?;
    if !canonical(suffix, &column.as_deref().map(String::as_str), control)? {
        return Err(invalid("noncanonical ACL tuple key"));
    }
    control.check()?;
    let (relation, mut memory) = relation.into_parts();
    let column = column.map(|column| {
        let (column, reservation) = column.into_parts();
        memory.absorb(reservation);
        column
    });
    Ok(Budgeted::new((relation, column), memory))
}

fn column(
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<String>>> {
    let mut reader = JsonReader::from_slice(bytes, control.memory(), control.cancellation());
    let value = super::input::next(&mut reader)?;
    let column = match value {
        JsonToken::Null => None,
        JsonToken::String(value) => Some(
            decode_json_string(value, control.memory(), control.cancellation())
                .map_err(super::input::json_error)?,
        ),
        _ => {
            return Err(super::input::invalid(
                "expected an optional ACL column name",
            ))
        }
    };
    if reader
        .next_event()
        .map_err(super::input::json_error)?
        .is_some()
    {
        return Err(super::input::invalid("trailing ACL column JSON"));
    }
    Ok(column)
}

fn canonical(
    expected: &[u8],
    value: &impl serde::Serialize,
    control: &StorageReadControl,
) -> StorageBackendResult<bool> {
    let mut output = Comparison {
        expected,
        offset: 0,
        matches: true,
        control,
    };
    let result = serde_json::to_writer(&mut output, value);
    control.check()?;
    result?;
    Ok(output.matches && output.offset == expected.len())
}

struct Comparison<'a> {
    expected: &'a [u8],
    offset: usize,
    matches: bool,
    control: &'a StorageReadControl,
}

impl Write for Comparison<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for chunk in bytes.chunks(4096) {
            self.control
                .check()
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::Other))?;
            let Some(end) = self.offset.checked_add(chunk.len()) else {
                return Err(std::io::ErrorKind::InvalidData.into());
            };
            self.matches &= self.expected.get(self.offset..end) == Some(chunk);
            self.offset = end;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

#[cfg(test)]
mod tests;
