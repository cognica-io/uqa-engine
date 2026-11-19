//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema privileges share role, recipient and allocation rules with other catalog authority.

use super::{
    invalid,
    paths::{decode, optional},
    projected, Input, JsonToken, Result,
};
use crate::{
    catalog_role::BoundAclEntry,
    catalog_schema::{
        BoundSchemaRow, SchemaAclEntry, SchemaPrivileges, SchemaRow, SchemaTupleIdentity,
    },
    memory::{Budgeted, BudgetedString, MemoryBudget, MemoryReservation},
    CancellationToken,
};

type Entry = BoundAclEntry<SchemaPrivileges>;

/// Decode native schema ACL text with the existing direct JSON struct semantics.
pub fn entries(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Option<Vec<Entry>>>> {
    decode(bytes, memory, cancellation, bound)
}

/// Decode legacy schema recipients and optional grantors without changing their default rules.
pub fn legacy_entries(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Option<Vec<SchemaAclEntry>>>> {
    decode(bytes, memory, cancellation, named)
}

/// Decode bound schema ACLs after the enclosing catalog's ordered last-value projection.
pub fn projected_entries(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Option<Vec<Entry>>>> {
    projected::decode(bytes, memory, cancellation, bound)
}

/// Decode named schema ACLs after complete syntax validation and ordered last-value projection.
pub fn projected_legacy_entries(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Option<Vec<SchemaAclEntry>>>> {
    projected::decode(bytes, memory, cancellation, named)
}

/// Preserve the stored tuple's projected struct fields; semantic validity remains with the catalog owner.
pub fn projected_tuple_identity(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<SchemaTupleIdentity> {
    projected::decode(bytes, memory, cancellation, |input, first| {
        input.schema_tuple(first)
    })
}

/// Decode the current schema row after the catalog envelope selects its format. Missing and null ACLs retain the ordinary owner-only default.
pub fn projected_bound_row(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<BoundSchemaRow>> {
    projected::decode(bytes, memory, cancellation, |input, first| {
        let mut name = None;
        let mut tuple = None;
        let mut owner = None;
        let mut acl = Budgeted::new(None, memory.empty_reservation());
        input.fields(
            first,
            ["name", "tuple", "role_owner", "acl"],
            [true, false, true, false],
            |input, index, value| {
                match index {
                    0 => name = Some(input.schema_text(value)?),
                    1 if value == JsonToken::Null => (),
                    1 => tuple = Some(input.schema_tuple(value)?),
                    2 => owner = Some(input.role(value)?),
                    3 => acl = bound(input, value)?,
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        let mut lease = memory.empty_reservation();
        Ok(Budgeted::new(
            BoundSchemaRow {
                name: take(name.expect("required schema name"), &mut lease),
                tuple,
                role_owner: owner.expect("required schema owner"),
                acl: take(acl, &mut lease),
            },
            lease,
        ))
    })
}

/// Decode the original named schema row, including positional representations and the default owner. No role binding or initial catalog migration occurs here.
pub fn projected_legacy_row(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<SchemaRow>> {
    projected::decode(bytes, memory, cancellation, |input, first| {
        let mut name = None;
        let mut owner = None;
        let mut acl = Budgeted::new(None, memory.empty_reservation());
        input.fields(
            first,
            ["name", "role_owner", "acl"],
            [true, false, false],
            |input, index, value| {
                match index {
                    0 => name = Some(input.schema_text(value)?),
                    1 => owner = Some(input.schema_text(value)?),
                    2 => acl = named(input, value)?,
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        let owner = if let Some(owner) = owner {
            owner
        } else {
            let mut name = BudgetedString::new(memory);
            name.push_str(crate::catalog_schema::DEFAULT_SCHEMA_ROLE_OWNER)?;
            let (name, lease) = name.into_parts();
            Budgeted::new(name, lease)
        };
        let mut lease = memory.empty_reservation();
        Ok(Budgeted::new(
            SchemaRow {
                name: take(name.expect("required schema name"), &mut lease),
                role_owner: take(owner, &mut lease),
                acl: take(acl, &mut lease),
            },
            lease,
        ))
    })
}

fn take<T>(value: Budgeted<T>, memory: &mut MemoryReservation) -> T {
    let (value, lease) = value.into_parts();
    memory.absorb(lease);
    value
}

fn bound<'a>(
    input: &mut Input<'a, '_, '_>,
    first: JsonToken<'a>,
) -> Result<Budgeted<Option<Vec<Entry>>>> {
    optional(input, first, |input, first| {
        let entry = input.entry::<SchemaPrivileges>(first)?;
        Ok(Budgeted::new(entry, input.memory.empty_reservation()))
    })
}

fn named<'a>(
    input: &mut Input<'a, '_, '_>,
    first: JsonToken<'a>,
) -> Result<Budgeted<Option<Vec<SchemaAclEntry>>>> {
    optional(input, first, |input, first| {
        input
            .legacy_fields::<SchemaPrivileges>(first)?
            .try_map(|entry| {
                Ok(SchemaAclEntry {
                    role: entry.role,
                    grantor: entry.grantor,
                    privileges: entry.privileges,
                    grant_options: entry.grant_options,
                })
            })
    })
}

impl super::PrivilegeInput for SchemaPrivileges {
    fn read<'a>(input: &mut Input<'a, '_, '_>, value: JsonToken<'a>) -> Result<Self> {
        input.schema_privileges(value)
    }
}

impl<'a> Input<'a, '_, '_> {
    fn schema_text(&self, value: JsonToken<'_>) -> Result<Budgeted<String>> {
        let JsonToken::String(value) = value else {
            return Err(invalid("expected a schema text field"));
        };
        crate::json::decode_json_string(value, self.memory, self.cancellation)
    }

    fn schema_privileges(&mut self, first: JsonToken<'a>) -> Result<SchemaPrivileges> {
        let mut flags = [false; 2];
        self.fields(first, ["usage", "create"], [false; 2], |_, index, value| {
            let JsonToken::Bool(value) = value else {
                return Err(invalid("expected a privilege flag"));
            };
            flags[index] = value;
            Ok(())
        })?;
        let [usage, create] = flags;
        Ok(SchemaPrivileges { usage, create })
    }

    fn schema_tuple(&mut self, first: JsonToken<'a>) -> Result<SchemaTupleIdentity> {
        let mut oid = 0;
        let mut object_id = [0; 16];
        let mut revision = [0; 16];
        self.fields_with_unknown(
            first,
            ["oid", "object_id", "revision"],
            [true; 3],
            true,
            |input, index, value| {
                match index {
                    0 => oid = input.number(value)?,
                    1 => object_id = input.identity(value)?,
                    2 => revision = input.identity(value)?,
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        Ok(SchemaTupleIdentity {
            oid,
            object_id,
            revision,
        })
    }
}

#[cfg(test)]
use super::reader;
#[cfg(test)]
mod tests;
