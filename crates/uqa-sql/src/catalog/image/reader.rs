//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One bounded cursor covers native role, membership, schema and database authority images.

use super::fields::{attribute_bit, Field, Fields};
use crate::{
    catalog::{
        named_definitions::CapturedDefinitionIter,
        roles::{RoleDefinition, RoleMembership, RoleMembershipKey},
        security::{
            database::{BoundDatabaseSecurity, DatabasePrivileges},
            BoundSchemaSecurity,
        },
    },
    SQLError,
};
use std::{io::Write, slice::Iter};
use uqa_core::{
    catalog_role::BoundAclEntry, catalog_schema::SchemaPrivileges, image_io::ImageOutput,
    CancellationToken,
};

pub(super) enum Entries<'a> {
    Roles(CapturedDefinitionIter<'a, String, RoleDefinition>),
    Memberships(CapturedDefinitionIter<'a, RoleMembershipKey, RoleMembership>),
    Schemas(CapturedDefinitionIter<'a, String, BoundSchemaSecurity>),
    Database(Option<&'a BoundDatabaseSecurity>),
}
enum Acl<'a> {
    None,
    Schema(Iter<'a, BoundAclEntry<SchemaPrivileges>>),
    Database(Iter<'a, BoundAclEntry<DatabasePrivileges>>),
}

enum State {
    Active,
    Complete,
    Failed,
}

/// A borrowed immutable image cursor with fixed state and no payload allocation. Its source and capture lease must remain alive through the borrow. Cancellation is checked before every field and at most 4 KiB between payload copies.
pub struct CatalogImageReader<'a> {
    entries: Entries<'a>,
    acl: Acl<'a>,
    fields: Fields<'a>,
    field: Field<'a>,
    offset: usize,
    cancellation: &'a CancellationToken,
    state: State,
}

impl<'a> CatalogImageReader<'a> {
    pub(super) fn new(
        entries: Entries<'a>,
        magic: [u8; 8],
        count: usize,
        cancellation: &'a CancellationToken,
    ) -> Result<Self, SQLError> {
        cancellation.check()?;
        let mut fields = Fields::new();
        fields.scalar(magic);
        fields.count(count);
        Ok(Self {
            entries,
            acl: Acl::None,
            fields,
            field: Field::Bytes(&[]),
            offset: 0,
            cancellation,
            state: State::Active,
        })
    }
    fn next_acl(&mut self) -> bool {
        match &mut self.acl {
            Acl::Schema(acl) => {
                if let Some(entry) = acl.next() {
                    self.fields.acl(entry);
                    return true;
                }
            }
            Acl::Database(acl) => {
                if let Some(entry) = acl.next() {
                    self.fields.acl(entry);
                    return true;
                }
            }
            Acl::None => {}
        }
        self.acl = Acl::None;
        false
    }
    fn next_record(&mut self) -> bool {
        self.fields = Fields::new();
        if self.next_acl() {
            return true;
        }
        match &mut self.entries {
            Entries::Roles(entries) => {
                let Some((key, role)) = entries.next() else {
                    return false;
                };
                let RoleDefinition {
                    oid,
                    object_id,
                    revision,
                    name,
                    attributes,
                    connection_limit,
                } = role;
                self.fields.text(key);
                self.fields.scalar(oid.to_le_bytes());
                self.fields.scalar(*object_id);
                self.fields.scalar(revision.to_le_bytes());
                self.fields.text(name);
                let bits = attributes
                    .iter()
                    .fold(0, |bits, attribute| bits | attribute_bit(*attribute));
                self.fields.scalar([bits]);
                self.fields.scalar(connection_limit.to_le_bytes());
            }
            Entries::Memberships(entries) => {
                let Some((key, membership)) = entries.next() else {
                    return false;
                };
                let RoleMembershipKey {
                    role,
                    member,
                    grantor,
                } = key;
                for identity in [role, member, grantor] {
                    self.fields.identity(*identity);
                }
                let RoleMembership {
                    oid,
                    role,
                    member,
                    grantor,
                    admin_option,
                    inherit_option,
                    set_option,
                } = membership;
                self.fields.scalar(oid.to_le_bytes());
                for binding in [role, member, grantor] {
                    self.fields.binding(binding);
                }
                self.fields.scalar([u8::from(*admin_option)
                    | (u8::from(*inherit_option) << 1)
                    | (u8::from(*set_option) << 2)]);
            }
            Entries::Schemas(entries) => {
                let Some((key, schema)) = entries.next() else {
                    return false;
                };
                let BoundSchemaSecurity {
                    tuple,
                    role_owner,
                    acl,
                } = schema;
                self.fields.text(key);
                self.fields.flag(tuple.is_some());
                if let Some(tuple) = tuple {
                    let uqa_core::catalog_schema::SchemaTupleIdentity {
                        oid,
                        object_id,
                        revision,
                    } = tuple;
                    self.fields.scalar(oid.to_le_bytes());
                    self.fields.scalar(*object_id);
                    self.fields.scalar(*revision);
                }
                self.fields.identity(*role_owner);
                self.fields.flag(acl.is_some());
                if let Some(acl) = acl {
                    self.fields.count(acl.len());
                    self.acl = Acl::Schema(acl.iter());
                }
            }
            Entries::Database(entry) => {
                let Some(database) = entry.take() else {
                    return false;
                };
                let BoundDatabaseSecurity { role_owner, acl } = database;
                self.fields.identity(*role_owner);
                self.fields.flag(acl.is_some());
                if let Some(acl) = acl {
                    self.fields.count(acl.len());
                    self.acl = Acl::Database(acl.iter());
                }
            }
        }
        true
    }
    /// Empty output consumes no fields. Errors permanently retire this cursor; partially written artifacts must be discarded.
    pub fn read(&mut self, output: &mut [u8]) -> Result<usize, SQLError> {
        match std::mem::replace(&mut self.state, State::Failed) {
            State::Failed => Err(super::invalid("retired image reader")),
            State::Complete => {
                self.state = State::Complete;
                Ok(0)
            }
            State::Active => {
                let count = self.read_inner(output)?;
                self.state = if count == 0 && !output.is_empty() {
                    State::Complete
                } else {
                    State::Active
                };
                Ok(count)
            }
        }
    }
    fn read_inner(&mut self, output: &mut [u8]) -> Result<usize, SQLError> {
        self.cancellation.check()?;
        let limit = output.len().min(16 * 1024);
        let mut written = 0;
        while written < limit {
            self.cancellation.check()?;
            let bytes = self.field.bytes();
            if self.offset < bytes.len() {
                let length = (bytes.len() - self.offset).min(limit - written).min(4096);
                output[written..written + length]
                    .copy_from_slice(&bytes[self.offset..self.offset + length]);
                self.offset += length;
                written += length;
            } else if let Some(field) = self.fields.next() {
                self.field = field;
                self.offset = 0;
            } else if !self.next_record() {
                break;
            }
        }
        self.cancellation.check()?;
        Ok(written)
    }
    pub fn write_to(&mut self, writer: &mut dyn Write) -> Result<u64, SQLError> {
        let mut output = ImageOutput {
            writer,
            cancellation: self.cancellation,
            written: 0,
        };
        let mut bytes = [0; 4096];
        loop {
            let length = self.read(&mut bytes)?;
            if length == 0 {
                return Ok(output.written);
            }
            self.state = State::Failed;
            output.bytes(&bytes[..length])?;
            self.state = State::Active;
        }
    }
}
