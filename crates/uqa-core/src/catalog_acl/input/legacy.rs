//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Legacy named ACL paths retain their buffers until catalog role binding completes.

use super::{invalid, reader, Input, JsonToken, Result};
use crate::{
    catalog_acl::{AclGrantee, TableAclEntry},
    json::decode_json_string,
    memory::{Budgeted, MemoryBudget, MemoryError, OwnedMap},
    CancellationToken,
};

type Columns = OwnedMap<String, Vec<TableAclEntry>>;

/// Decode the existing optional named ACL, preserving PUBLIC, named recipients, default grants and original path order.
pub fn legacy_entries(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Option<Vec<TableAclEntry>>>> {
    let mut tokens = reader(bytes, memory, cancellation);
    let mut input = Input::new(&mut tokens, memory, cancellation);
    let token = input.next()?;
    let result = if token == JsonToken::Null {
        Budgeted::new(None, memory.empty_reservation())
    } else {
        let (entries, memory) = input.legacy_acl(token)?.into_parts();
        Budgeted::new(Some(entries), memory)
    };
    input.end()?;
    Ok(result)
}

/// Decode named column ACLs directly into their native ordered nodes. Duplicate names retain the original last-value rule and retire the replaced paths before returning their allowance.
pub fn legacy_column_entries(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Columns>> {
    let mut tokens = reader(bytes, memory, cancellation);
    let mut input = Input::new(&mut tokens, memory, cancellation);
    if input.next()? != JsonToken::StartObject {
        return Err(invalid("expected a column ACL map"));
    }
    let mut reservation = memory.empty_reservation();
    let mut columns = Columns::new();
    loop {
        let token = input.next()?;
        if token == JsonToken::EndObject {
            break;
        }
        let JsonToken::Key(name) = token else {
            return Err(invalid("expected a column ACL name"));
        };
        let name = decode_json_string(name, memory, cancellation)?;
        let token = input.next()?;
        let entries = input.legacy_acl(token)?;
        if let Some(previous) = columns.get_mut(name.as_str()) {
            let bytes = acl_bytes(previous, cancellation)?;
            let (entries, lease) = entries.into_parts();
            *previous = entries;
            drop(reservation.split(bytes));
            reservation.absorb(lease);
        } else {
            reservation.grow(Columns::entry_bytes())?;
            let (name, name_memory) = name.into_parts();
            let (entries, entries_memory) = entries.into_parts();
            reservation.absorb(name_memory);
            reservation.absorb(entries_memory);
            columns.insert(name, entries);
        }
    }
    input.end()?;
    Ok(Budgeted::new(columns, reservation))
}

fn acl_bytes(entries: &Vec<TableAclEntry>, cancellation: &CancellationToken) -> Result<usize> {
    let mut bytes = entries
        .capacity()
        .checked_mul(size_of::<TableAclEntry>())
        .ok_or(MemoryError::SizeOverflow)?;
    for entry in entries {
        cancellation.check()?;
        let role = match &entry.role {
            AclGrantee::Public => 0,
            AclGrantee::Role(name) => name.capacity(),
        };
        bytes = bytes
            .checked_add(role)
            .and_then(|bytes| bytes.checked_add(entry.grantor.as_ref().map_or(0, String::capacity)))
            .ok_or(MemoryError::SizeOverflow)?;
    }
    Ok(bytes)
}

pub(super) struct LegacyEntry<P> {
    pub role: AclGrantee,
    pub grantor: Option<String>,
    pub privileges: P,
    pub grant_options: P,
}

impl<'a> Input<'a, '_, '_> {
    pub(super) fn legacy_acl(
        &mut self,
        first: JsonToken<'a>,
    ) -> Result<Budgeted<Vec<TableAclEntry>>> {
        self.acl_with(first, Self::legacy_entry)
    }

    fn legacy_entry(&mut self, first: JsonToken<'a>) -> Result<Budgeted<TableAclEntry>> {
        self.legacy_fields::<super::TablePrivileges>(first)?
            .try_map(|entry| {
                Ok(TableAclEntry {
                    role: entry.role,
                    grantor: entry.grantor,
                    privileges: entry.privileges,
                    grant_options: entry.grant_options,
                })
            })
    }

    pub(super) fn legacy_fields<P: super::PrivilegeInput>(
        &mut self,
        first: JsonToken<'a>,
    ) -> Result<Budgeted<LegacyEntry<P>>> {
        let mut role = None;
        let mut grantor = None;
        let mut privileges = P::default();
        let mut grant_options = P::default();
        self.fields(
            first,
            ["role", "grantor", "privileges", "grant_options"],
            [true, false, false, false],
            |input, index, value| {
                match index {
                    0 => role = Some(input.legacy_grantee(value)?),
                    1 if value == JsonToken::Null => (),
                    1 => grantor = Some(input.legacy_name(value)?),
                    2 => privileges = P::read(input, value)?,
                    3 => grant_options = P::read(input, value)?,
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        let (role, mut memory) = role.expect("required recipient").into_parts();
        let grantor = grantor.map(|name| {
            let (name, reservation) = name.into_parts();
            memory.absorb(reservation);
            name
        });
        Ok(Budgeted::new(
            LegacyEntry {
                role,
                grantor,
                privileges,
                grant_options,
            },
            memory,
        ))
    }

    fn legacy_name(&self, value: JsonToken<'_>) -> Result<Budgeted<String>> {
        let JsonToken::String(value) = value else {
            return Err(invalid("expected an ACL role name"));
        };
        decode_json_string(value, self.memory, self.cancellation)
    }

    fn legacy_grantee(&mut self, first: JsonToken<'a>) -> Result<Budgeted<AclGrantee>> {
        if matches!(first, JsonToken::String(_)) {
            let name = self.legacy_name(first)?;
            if name.as_str() == "PUBLIC" {
                return Ok(Budgeted::new(
                    AclGrantee::Public,
                    self.memory.empty_reservation(),
                ));
            }
            let (name, memory) = name.into_parts();
            return Ok(Budgeted::new(AclGrantee::Role(name), memory));
        }
        let mut public = false;
        let mut name = None;
        self.fields_with_unknown(
            first,
            ["kind", "name"],
            [true, first == JsonToken::StartArray],
            true,
            |input, index, value| {
                match index {
                    0 => {
                        public = match input.legacy_name(value)?.as_str() {
                            "public" => true,
                            "role" => false,
                            _ => return Err(invalid("unknown ACL recipient kind")),
                        };
                    }
                    1 if value == JsonToken::Null => (),
                    1 => name = Some(input.legacy_name(value)?),
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        if public {
            if name.is_some() {
                return Err(invalid("PUBLIC recipient has a name"));
            }
            Ok(Budgeted::new(
                AclGrantee::Public,
                self.memory.empty_reservation(),
            ))
        } else {
            let (name, memory) = name
                .ok_or_else(|| invalid("missing ACL recipient name"))?
                .into_parts();
            Ok(Budgeted::new(AclGrantee::Role(name), memory))
        }
    }
}

#[cfg(test)]
use crate::catalog_acl::TablePrivileges;

#[cfg(test)]
mod tests;
