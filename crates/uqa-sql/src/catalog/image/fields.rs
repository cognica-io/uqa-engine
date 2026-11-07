//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed field descriptors borrow original text and encode scalar identities explicitly.

use crate::{
    ast::{ObjectAclEntry, RelationPersistence, RoleAttribute},
    catalog::{
        roles::{
            identity::RoleBinding, RoleDefinition, RoleIdentity, RoleMembership, RoleMembershipKey,
        },
        security::{
            database::DatabasePrivileges, sequence_registry::SequenceAuthority, BoundSchemaSecurity,
        },
    },
    SQLError,
};
use uqa_core::{
    catalog_acl::TablePrivileges, catalog_role::BoundAclEntry, catalog_schema::SchemaPrivileges,
    catalog_sequence::SequencePrivileges, RelationIdentity,
};

mod column_types;
mod foreign_wrappers;
mod functions;
mod metadata;
mod structured_types;

#[derive(Clone, Copy)]
pub(super) enum Field<'a> {
    Bytes(&'a [u8]),
    Scalar([u8; 40], usize),
}
impl Field<'_> {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::Scalar(bytes, length) => &bytes[..*length],
        }
    }
}

pub(super) struct Fields<'a> {
    values: [Field<'a>; 24],
    count: usize,
    position: usize,
}
impl<'a> Fields<'a> {
    pub fn new() -> Self {
        Self {
            values: [Field::Bytes(&[]); 24],
            count: 0,
            position: 0,
        }
    }
    fn push(&mut self, field: Field<'a>) {
        self.values[self.count] = field;
        self.count += 1;
    }
    pub fn scalar<const N: usize>(&mut self, bytes: [u8; N]) {
        let mut value = [0; 40];
        value[..N].copy_from_slice(&bytes);
        self.push(Field::Scalar(value, N));
    }
    pub fn count(&mut self, count: usize) {
        self.scalar((count as u64).to_le_bytes());
    }
    pub fn flag(&mut self, value: bool) {
        self.scalar([u8::from(value)]);
    }
    pub fn text(&mut self, value: &'a str) {
        self.blob(value.as_bytes());
    }
    pub fn bytes(&mut self, value: &'a [u8]) {
        self.push(Field::Bytes(value));
    }
    pub fn blob(&mut self, value: &'a [u8]) {
        self.count(value.len());
        self.push(Field::Bytes(value));
    }
    pub fn optional_text(&mut self, value: Option<&'a str>) {
        self.flag(value.is_some());
        if let Some(value) = value {
            self.text(value);
        }
    }
    pub fn optional_u32(&mut self, value: Option<u32>) {
        self.flag(value.is_some());
        if let Some(value) = value {
            self.scalar(value.to_le_bytes());
        }
    }
    pub fn optional_incarnation(&mut self, value: Option<[u8; 16]>) {
        self.flag(value.is_some());
        if let Some(value) = value {
            self.scalar(value);
        }
    }
    pub fn identity(&mut self, value: RoleIdentity) {
        self.scalar(value.oid.to_le_bytes());
        self.scalar(value.object_id);
    }
    pub fn binding(&mut self, value: &'a RoleBinding) {
        let RoleBinding {
            name,
            oid,
            object_id,
            ..
        } = value;
        self.text(name);
        self.scalar(oid.to_le_bytes());
        self.scalar(*object_id);
    }
    pub fn acl<P: Privileges>(&mut self, value: &BoundAclEntry<P>) {
        let BoundAclEntry {
            role,
            grantor,
            privileges,
            grant_options,
        } = value;
        self.flag(role.is_some());
        if let Some(role) = role {
            self.identity(*role);
        }
        self.identity(*grantor);
        self.scalar([privileges.bits(), grant_options.bits()]);
    }
    pub fn object_acl(&mut self, value: &ObjectAclEntry) {
        let ObjectAclEntry {
            role,
            grantor,
            grant_option,
        } = value;
        self.flag(role.is_some());
        if let Some(role) = role {
            self.identity(*role);
        }
        self.identity(*grantor);
        self.flag(*grant_option);
    }
    pub fn schema(&mut self, key: &'a str, schema: &'a BoundSchemaSecurity) {
        let BoundSchemaSecurity {
            tuple,
            role_owner,
            acl,
        } = schema;
        self.text(key);
        self.flag(tuple.is_some());
        if let Some(tuple) = tuple {
            let uqa_core::catalog_schema::SchemaTupleIdentity {
                oid,
                object_id,
                revision,
            } = tuple;
            self.scalar(oid.to_le_bytes());
            self.scalar(*object_id);
            self.scalar(*revision);
        }
        self.identity(*role_owner);
        self.flag(acl.is_some());
        if let Some(acl) = acl {
            self.count(acl.len());
        }
    }
    pub fn sequence_authority(
        &mut self,
        relation: &'a RelationIdentity,
        authority: &'a SequenceAuthority,
    ) -> Result<(), SQLError> {
        self.text(&relation.schema);
        self.text(&relation.name);
        self.scalar([match authority.persistence() {
            RelationPersistence::Permanent => 0,
            RelationPersistence::Unlogged => 1,
            RelationPersistence::Temporary => {
                return Err(super::invalid("temporary sequence authority"));
            }
        }]);
        let security = authority.security();
        self.identity(security.role_owner);
        self.flag(security.acl.is_some());
        if let Some(acl) = &security.acl {
            self.count(acl.len());
        }
        Ok(())
    }
    pub fn role(&mut self, key: &'a str, role: &'a RoleDefinition) {
        let RoleDefinition {
            oid,
            object_id,
            revision,
            name,
            attributes,
            connection_limit,
        } = role;
        self.text(key);
        self.scalar(oid.to_le_bytes());
        self.scalar(*object_id);
        self.scalar(revision.to_le_bytes());
        self.text(name);
        let bits = attributes
            .iter()
            .fold(0, |bits, attribute| bits | attribute_bit(*attribute));
        self.scalar([bits]);
        self.scalar(connection_limit.to_le_bytes());
    }
    pub fn membership(&mut self, key: &RoleMembershipKey, membership: &'a RoleMembership) {
        let RoleMembershipKey {
            role,
            member,
            grantor,
        } = key;
        for identity in [role, member, grantor] {
            self.identity(*identity);
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
        self.scalar(oid.to_le_bytes());
        for binding in [role, member, grantor] {
            self.binding(binding);
        }
        self.scalar([u8::from(*admin_option)
            | (u8::from(*inherit_option) << 1)
            | (u8::from(*set_option) << 2)]);
    }
    pub fn next(&mut self) -> Option<Field<'a>> {
        if self.position == self.count {
            return None;
        }
        let field = self.values[self.position];
        self.position += 1;
        Some(field)
    }
}

pub(super) fn attribute_bit(value: RoleAttribute) -> u8 {
    match value {
        RoleAttribute::Superuser => 1,
        RoleAttribute::Inherit => 2,
        RoleAttribute::CreateRole => 4,
        RoleAttribute::CreateDb => 8,
        RoleAttribute::Login => 16,
        RoleAttribute::Replication => 32,
        RoleAttribute::BypassRls => 64,
    }
}

pub(super) trait Privileges: Sized {
    fn bits(&self) -> u8;
    fn from_bits(bits: u8) -> Option<Self>;
}
impl Privileges for SequencePrivileges {
    fn bits(&self) -> u8 {
        let Self {
            select,
            update,
            usage,
        } = self;
        u8::from(*select) | (u8::from(*update) << 1) | (u8::from(*usage) << 2)
    }
    fn from_bits(bits: u8) -> Option<Self> {
        (bits & !7 == 0).then_some(Self {
            select: bits & 1 != 0,
            update: bits & 2 != 0,
            usage: bits & 4 != 0,
        })
    }
}
impl Privileges for SchemaPrivileges {
    fn bits(&self) -> u8 {
        let Self { usage, create } = self;
        u8::from(*usage) | (u8::from(*create) << 1)
    }
    fn from_bits(bits: u8) -> Option<Self> {
        (bits & !3 == 0).then_some(Self {
            usage: bits & 1 != 0,
            create: bits & 2 != 0,
        })
    }
}
impl Privileges for DatabasePrivileges {
    fn bits(&self) -> u8 {
        let Self {
            connect,
            create,
            temporary,
        } = self;
        u8::from(*connect) | (u8::from(*create) << 1) | (u8::from(*temporary) << 2)
    }
    fn from_bits(bits: u8) -> Option<Self> {
        (bits & !7 == 0).then_some(Self {
            connect: bits & 1 != 0,
            create: bits & 2 != 0,
            temporary: bits & 4 != 0,
        })
    }
}

impl Privileges for TablePrivileges {
    fn bits(&self) -> u8 {
        let Self {
            select,
            insert,
            update,
            delete,
            truncate,
            references,
            trigger,
            maintain,
        } = self;
        u8::from(*select)
            | (u8::from(*insert) << 1)
            | (u8::from(*update) << 2)
            | (u8::from(*delete) << 3)
            | (u8::from(*truncate) << 4)
            | (u8::from(*references) << 5)
            | (u8::from(*trigger) << 6)
            | (u8::from(*maintain) << 7)
    }
    fn from_bits(bits: u8) -> Option<Self> {
        Some(Self {
            select: bits & 1 != 0,
            insert: bits & 2 != 0,
            update: bits & 4 != 0,
            delete: bits & 8 != 0,
            truncate: bits & 16 != 0,
            references: bits & 32 != 0,
            trigger: bits & 64 != 0,
            maintain: bits & 128 != 0,
        })
    }
}
