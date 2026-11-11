//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Controlled JSON input for durable role identities and bound relation ACL paths.

mod fields;

use super::TablePrivileges;
use crate::{
    catalog_role::{BoundAclEntry, RoleIdentity},
    json::{decode_json_string, JsonReadError, JsonReader, JsonToken},
    memory::{Budgeted, BudgetedVec, MemoryBudget, OwnedMap},
    CancellationToken,
};

type Entry = BoundAclEntry<TablePrivileges>;
type Columns = OwnedMap<String, Vec<Entry>>;
type Result<T> = std::result::Result<T, JsonReadError>;

/// Decode the existing role-identity JSON shape without constructing an intermediate value tree. Identity validity remains the catalog validator's responsibility.
pub fn role_identity(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<RoleIdentity> {
    let mut input = Input::new(bytes, memory, cancellation);
    let token = input.next()?;
    let role = input.role(token)?;
    input.end()?;
    Ok(role)
}

/// Decode an optional ordered relation ACL. Every entry buffer is admitted before allocation; explicit null and empty ACLs remain distinct.
pub fn entries(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Option<Vec<Entry>>>> {
    let mut input = Input::new(bytes, memory, cancellation);
    let token = input.next()?;
    let value = if token == JsonToken::Null {
        Budgeted::new(None, memory.empty_reservation())
    } else {
        let (entries, memory) = input.acl(token)?.into_parts();
        Budgeted::new(Some(entries), memory)
    };
    input.end()?;
    Ok(value)
}

/// Decode a column ACL map with the existing last-value collision rule. Original names and path buffers enter the final map once; replacement releases the superseded buffer's reservation.
pub fn column_entries(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Columns>> {
    let mut input = Input::new(bytes, memory, cancellation);
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
        let entries = input.acl(token)?;
        if let Some(previous) = columns.get_mut(name.as_str()) {
            let bytes = previous.capacity() * size_of::<Entry>();
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

struct Input<'a, 'c> {
    reader: JsonReader<'a, 'c>,
    memory: &'c MemoryBudget,
    cancellation: &'c CancellationToken,
}

impl<'a, 'c> Input<'a, 'c> {
    fn new(bytes: &'a [u8], memory: &'c MemoryBudget, cancellation: &'c CancellationToken) -> Self {
        Self {
            reader: JsonReader::from_slice(bytes, memory, cancellation)
                .with_ignored_string_escapes(),
            memory,
            cancellation,
        }
    }

    fn next(&mut self) -> Result<JsonToken<'a>> {
        self.reader
            .next_event()?
            .map(|event| event.token)
            .ok_or_else(|| invalid("unexpected end of authority JSON"))
    }

    fn end(&mut self) -> Result<()> {
        if self.reader.next_event()?.is_some() {
            return Err(invalid("trailing authority JSON"));
        }
        self.cancellation.check()?;
        Ok(())
    }

    fn role(&mut self, token: JsonToken<'a>) -> Result<RoleIdentity> {
        let mut oid = 0;
        let mut object_id = [0; 16];
        self.fields(
            token,
            ["oid", "object_id"],
            [true; 2],
            |input, index, value| {
                match index {
                    0 => oid = number(value)?,
                    1 => object_id = input.identity(value)?,
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        Ok(RoleIdentity { oid, object_id })
    }

    fn identity(&mut self, token: JsonToken<'a>) -> Result<[u8; 16]> {
        if token != JsonToken::StartArray {
            return Err(invalid("expected a catalog incarnation array"));
        }
        let mut identity = [0; 16];
        for byte in &mut identity {
            *byte = number(self.next()?)?;
        }
        if self.next()? != JsonToken::EndArray {
            return Err(invalid("invalid catalog incarnation width"));
        }
        Ok(identity)
    }

    fn acl(&mut self, token: JsonToken<'a>) -> Result<Budgeted<Vec<Entry>>> {
        if token != JsonToken::StartArray {
            return Err(invalid("expected an ACL path array"));
        }
        let mut entries = BudgetedVec::new(self.memory);
        loop {
            let token = self.next()?;
            if token == JsonToken::EndArray {
                break;
            }
            entries.push(self.entry(token)?)?;
        }
        let (entries, memory) = entries.into_parts();
        Ok(Budgeted::new(entries, memory))
    }

    fn entry(&mut self, token: JsonToken<'a>) -> Result<Entry> {
        let mut role = None;
        let mut grantor = RoleIdentity::BOOTSTRAP;
        let mut privileges = TablePrivileges::default();
        let mut grant_options = TablePrivileges::default();
        self.fields(
            token,
            ["role", "grantor", "privileges", "grant_options"],
            [true; 4],
            |input, index, value| {
                match index {
                    0 if value == JsonToken::Null => (),
                    0 => role = Some(input.role(value)?),
                    1 => grantor = input.role(value)?,
                    2 => privileges = input.privileges(value)?,
                    3 => grant_options = input.privileges(value)?,
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        Ok(Entry {
            role,
            grantor,
            privileges,
            grant_options,
        })
    }

    fn privileges(&mut self, token: JsonToken<'a>) -> Result<TablePrivileges> {
        let mut flags = [false; 8];
        self.fields(
            token,
            [
                "select",
                "insert",
                "update",
                "delete",
                "truncate",
                "references",
                "trigger",
                "maintain",
            ],
            [false; 8],
            |_, index, value| {
                let JsonToken::Bool(value) = value else {
                    return Err(invalid("expected a privilege flag"));
                };
                flags[index] = value;
                Ok(())
            },
        )?;
        let [select, insert, update, delete, truncate, references, trigger, maintain] = flags;
        Ok(TablePrivileges {
            select,
            insert,
            update,
            delete,
            truncate,
            references,
            trigger,
            maintain,
        })
    }
}

fn number<T: serde::de::DeserializeOwned>(token: JsonToken<'_>) -> Result<T> {
    let JsonToken::Number(value) = token else {
        return Err(invalid("expected an integer authority field"));
    };
    // Fixed-width scalar decoding preserves the durable numeric classification without string or container scratch.
    serde_json::from_str(value).map_err(|error| invalid(error.to_string()))
}

fn invalid(reason: impl Into<String>) -> JsonReadError {
    JsonReadError::Malformed {
        kind: "relation authority",
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests;
