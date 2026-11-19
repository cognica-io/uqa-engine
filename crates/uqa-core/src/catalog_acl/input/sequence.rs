//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence authority uses the shared role and ACL grammar with its own privilege carrier.

use super::{
    invalid,
    paths::{decode, optional},
    projected, Input, JsonToken, Result,
};
use crate::{
    catalog_role::BoundAclEntry,
    catalog_sequence::{SequenceAclEntry, SequencePrivileges},
    memory::{Budgeted, MemoryBudget},
    CancellationToken,
};

type Entry = BoundAclEntry<SequencePrivileges>;

/// Decode direct optional bound sequence ACLs, retaining null and empty paths separately.
pub fn entries(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Option<Vec<Entry>>>> {
    decode(bytes, memory, cancellation, bound)
}

/// Decode original named sequence ACLs with optional grantors and default privileges.
pub fn legacy_entries(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Option<Vec<SequenceAclEntry>>>> {
    decode(bytes, memory, cancellation, named)
}

/// Decode bound ACLs after the stored authority's ordered last-value projection.
pub fn projected_entries(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Option<Vec<Entry>>>> {
    projected::decode(bytes, memory, cancellation, bound)
}

/// Decode named ACLs after validating all source values, including overwritten fields.
pub fn projected_legacy_entries(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Option<Vec<SequenceAclEntry>>>> {
    projected::decode(bytes, memory, cancellation, named)
}

fn bound<'a>(
    input: &mut Input<'a, '_, '_>,
    first: JsonToken<'a>,
) -> Result<Budgeted<Option<Vec<Entry>>>> {
    optional(input, first, |input, first| {
        let entry = input.entry::<SequencePrivileges>(first)?;
        Ok(Budgeted::new(entry, input.memory.empty_reservation()))
    })
}

fn named<'a>(
    input: &mut Input<'a, '_, '_>,
    first: JsonToken<'a>,
) -> Result<Budgeted<Option<Vec<SequenceAclEntry>>>> {
    optional(input, first, |input, first| {
        input
            .legacy_fields::<SequencePrivileges>(first)?
            .try_map(|entry| {
                Ok(SequenceAclEntry {
                    role: entry.role,
                    grantor: entry.grantor,
                    privileges: entry.privileges,
                    grant_options: entry.grant_options,
                })
            })
    })
}

impl super::PrivilegeInput for SequencePrivileges {
    fn read<'a>(input: &mut Input<'a, '_, '_>, first: JsonToken<'a>) -> Result<Self> {
        let mut flags = [false; 3];
        input.fields(
            first,
            ["select", "update", "usage"],
            [false; 3],
            |_, index, value| {
                let JsonToken::Bool(value) = value else {
                    return Err(invalid("expected a privilege flag"));
                };
                flags[index] = value;
                Ok(())
            },
        )?;
        let [select, update, usage] = flags;
        Ok(Self {
            select,
            update,
            usage,
        })
    }
}

/// Consume one optional stored owner from a borrowed JSON prefix. Scalar identities remain fixed-size and unknown fields use the admitted shared reader.
pub fn owner_prefix(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Option<crate::catalog_sequence::SequenceOwner>> {
    use crate::catalog_sequence::{SequenceOwner, SequenceOwnerDependency};
    let mut reader = super::reader(bytes, memory, cancellation);
    let mut input = Input::new(&mut reader, memory, cancellation);
    let first = input.next()?;
    if first == JsonToken::Null {
        return Ok(None);
    }
    let (mut table, mut column) = ([0; 16], [0; 16]);
    let mut dependency = SequenceOwnerDependency::Automatic;
    input.fields(
        first,
        ["table_object_id", "column_object_id", "dependency"],
        [true, true, false],
        |input, index, token| {
            match index {
                0 => table = input.identity(token)?,
                1 => column = input.identity(token)?,
                2 => {
                    let object = token == JsonToken::StartObject;
                    let token = if object { input.next()? } else { token };
                    let (JsonToken::String(value) | JsonToken::Key(value)) = token else {
                        return Err(invalid("expected a sequence owner dependency"));
                    };
                    let text = crate::json::decode_json_string(value, memory, cancellation)?;
                    dependency = match text.as_str() {
                        "automatic" => SequenceOwnerDependency::Automatic,
                        "internal" => SequenceOwnerDependency::Internal,
                        _ => return Err(invalid("invalid sequence owner dependency")),
                    };
                    if object
                        && (input.next()? != JsonToken::Null
                            || input.next()? != JsonToken::EndObject)
                    {
                        return Err(invalid("invalid sequence owner dependency value"));
                    }
                }
                _ => unreachable!(),
            }
            Ok(())
        },
    )?;
    cancellation.check()?;
    Ok(Some(SequenceOwner {
        table_object_id: table,
        column_object_id: column,
        dependency,
    }))
}

#[cfg(test)]
mod tests;
