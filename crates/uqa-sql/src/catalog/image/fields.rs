//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed field descriptors borrow original text and encode scalar identities explicitly.

use crate::{
    ast::RoleAttribute,
    catalog::{
        roles::{identity::RoleBinding, RoleIdentity},
        security::database::DatabasePrivileges,
    },
};
use uqa_core::{catalog_role::BoundAclEntry, catalog_schema::SchemaPrivileges};

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
        self.count(value.len());
        self.push(Field::Bytes(value.as_bytes()));
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
