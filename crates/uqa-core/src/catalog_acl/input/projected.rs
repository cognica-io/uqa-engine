//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered last-value authority fields over borrowed JSON source spans.

use super::{invalid, Entry, Input, JsonReader, JsonToken, Result};
use crate::{
    catalog_acl::TableAclEntry,
    catalog_role::RoleIdentity,
    json::JsonObjectProjection,
    memory::{Budgeted, BudgetedString, MemoryBudget, OwnedMap},
    CancellationToken,
};

/// Decode a role after ordered last-value object projection and integer classification.
pub fn role_identity(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<RoleIdentity> {
    decode(bytes, memory, cancellation, |input, first| {
        input.role(first)
    })
}

/// Decode an optional bound ACL after validating all original JSON values, including overwritten fields.
pub fn entries(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Option<Vec<Entry>>>> {
    decode(bytes, memory, cancellation, |input, first| {
        input.optional_acl(first)
    })
}

/// Decode an optional named ACL with sorted last-value fields and the original recipient representations.
pub fn legacy_entries(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Option<Vec<TableAclEntry>>>> {
    decode(bytes, memory, cancellation, |input, first| {
        if first == JsonToken::Null {
            Ok(Budgeted::new(None, memory.empty_reservation()))
        } else {
            let (value, lease) = input.legacy_acl(first)?.into_parts();
            Ok(Budgeted::new(Some(value), lease))
        }
    })
}

/// Decode the final bound path at each column name; overwritten paths are syntax-validated without typed construction.
pub fn column_entries(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<OwnedMap<String, Vec<Entry>>>> {
    columns(bytes, memory, cancellation, bound_path)
}

/// Decode the final named path at each column name into admitted native nodes.
pub fn legacy_column_entries(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<OwnedMap<String, Vec<TableAclEntry>>>> {
    columns(bytes, memory, cancellation, |input, first| {
        input.legacy_acl(first)
    })
}

fn bound_path<'a>(
    input: &mut Input<'a, '_, '_>,
    first: JsonToken<'a>,
) -> Result<Budgeted<Vec<Entry>>> {
    input.acl(first)
}

fn columns<'a, 'c, T>(
    bytes: &'a [u8],
    memory: &'c MemoryBudget,
    cancellation: &'c CancellationToken,
    mut path: impl for<'r> FnMut(&mut Input<'a, 'c, 'r>, JsonToken<'a>) -> Result<Budgeted<Vec<T>>>,
) -> Result<Budgeted<OwnedMap<String, Vec<T>>>> {
    let projection = JsonObjectProjection::read(bytes, memory, cancellation)?;
    let mut lease = memory.empty_reservation();
    let mut columns = OwnedMap::new();
    for (name, bytes) in projection.iter() {
        cancellation.check()?;
        let mut owned_name = BudgetedString::new(memory);
        owned_name.push_str(name)?;
        let entries = decode(bytes, memory, cancellation, &mut path)?;
        lease.grow(OwnedMap::<String, Vec<T>>::entry_bytes())?;
        let (name, name_lease) = owned_name.into_parts();
        let (entries, entries_lease) = entries.into_parts();
        lease.absorb(name_lease);
        lease.absorb(entries_lease);
        columns.insert(name, entries);
    }
    Ok(Budgeted::new(columns, lease))
}

fn decode<'a, 'c, T>(
    bytes: &'a [u8],
    memory: &'c MemoryBudget,
    cancellation: &'c CancellationToken,
    operation: impl for<'r> FnOnce(&mut Input<'a, 'c, 'r>, JsonToken<'a>) -> Result<T>,
) -> Result<T> {
    let mut validation = JsonReader::from_slice(bytes, memory, cancellation).with_depth_limit(127);
    while validation.next_event()?.is_some() {}
    drop(validation);
    let mut tokens = JsonReader::from_slice(bytes, memory, cancellation);
    let mut input = Input::new(&mut tokens, memory, cancellation);
    input.projection = Some(bytes);
    let first = input.next()?;
    let value = operation(&mut input, first)?;
    input.end()?;
    Ok(value)
}

impl<'a, 'c> Input<'a, 'c, '_> {
    pub(super) fn projected_fields<const N: usize>(
        &mut self,
        first: JsonToken<'a>,
        names: [&str; N],
        required: [bool; N],
        deny_unknown: bool,
        mut field: impl for<'s, 'r> FnMut(&mut Input<'s, 'c, 'r>, usize, JsonToken<'s>) -> Result<()>,
    ) -> Result<()> {
        let start = self.range.start;
        self.skip(first)?;
        let bytes = &self.projection.expect("projected object")[start..self.range.end];
        let projection = JsonObjectProjection::read(bytes, self.memory, self.cancellation)?;
        let mut seen = [false; N];
        for (name, bytes) in projection.iter() {
            let Some(index) = names.iter().position(|expected| *expected == name) else {
                if deny_unknown {
                    return Err(invalid(format!("unknown field `{name}`")));
                }
                continue;
            };
            let mut tokens = JsonReader::from_slice(bytes, self.memory, self.cancellation);
            let mut input = Input::new(&mut tokens, self.memory, self.cancellation);
            input.projection = Some(bytes);
            let first = input.next()?;
            field(&mut input, index, first)?;
            input.end()?;
            seen[index] = true;
        }
        for (index, name) in names.into_iter().enumerate() {
            if required[index] && !seen[index] {
                return Err(invalid(format!("missing field `{name}`")));
            }
        }
        Ok(())
    }
}

pub(super) fn integer<T: serde::de::DeserializeOwned>(token: JsonToken<'_>) -> Result<T> {
    use serde::de::value::{Error, I64Deserializer, U64Deserializer};
    let JsonToken::Number(text) = token else {
        return Err(invalid("expected an integer authority field"));
    };
    // Integer JSON values classify as unsigned first, then signed. In particular, signed negative zero becomes integer zero before the typed conversion.
    let value = if let Ok(value) = text.parse::<u64>() {
        T::deserialize(U64Deserializer::<Error>::new(value))
    } else if let Ok(value) = text.parse::<i64>() {
        T::deserialize(I64Deserializer::<Error>::new(value))
    } else {
        return Err(invalid("invalid integer authority field"));
    };
    value.map_err(|error| invalid(error.to_string()))
}

#[cfg(test)]
mod tests;
