//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One bounded cursor covers native catalog images.

use super::fields::{Field, Fields};
use crate::{
    ast::{ColumnType, ObjectAclEntry},
    catalog::{
        composite_type::StoredComposite,
        enum_type::StoredEnum,
        foreign_server::ForeignServerDefinition,
        graph_oids::GraphCatalogOids,
        named_definitions::CapturedDefinitionIter,
        range_type::StoredRange,
        roles::{RoleDefinition, RoleMembership, RoleMembershipKey},
        security::{
            builtin_routines::BuiltinRoutineSecurity,
            database::{BoundDatabaseSecurity, DatabasePrivileges},
            system_relations::{SystemAcl, SystemRelationSecurity},
            BoundSchemaSecurity,
        },
        shell_type::StoredShellType,
    },
    SQLError,
};

mod metadata;
use metadata::Tail;
use std::{io::Write, slice::Iter};
use uqa_core::{
    catalog_acl::TablePrivileges, catalog_role::BoundAclEntry, catalog_schema::SchemaPrivileges,
    image_io::ImageOutput, memory::BudgetedMapIter, CancellationToken, RelationIdentity,
};

pub(super) enum Entries<'a> {
    Roles(CapturedDefinitionIter<'a, String, RoleDefinition>),
    Memberships(CapturedDefinitionIter<'a, RoleMembershipKey, RoleMembership>),
    Schemas(CapturedDefinitionIter<'a, String, BoundSchemaSecurity>),
    Database(Option<&'a BoundDatabaseSecurity>),
    BuiltinRoutines(CapturedDefinitionIter<'a, u32, BuiltinRoutineSecurity>),
    SystemRelations(CapturedDefinitionIter<'a, RelationIdentity, SystemRelationSecurity>),
    Composites(CapturedDefinitionIter<'a, String, StoredComposite>),
    Ranges(CapturedDefinitionIter<'a, String, StoredRange>),
    Enums(CapturedDefinitionIter<'a, String, StoredEnum>),
    Shells(CapturedDefinitionIter<'a, String, StoredShellType>),
    ForeignServers(CapturedDefinitionIter<'a, String, ForeignServerDefinition>),
    GraphOids(CapturedDefinitionIter<'a, String, GraphCatalogOids>),
}
enum Acl<'a> {
    None,
    Schema(Iter<'a, BoundAclEntry<SchemaPrivileges>>),
    Database(Iter<'a, BoundAclEntry<DatabasePrivileges>>),
    Object(Iter<'a, ObjectAclEntry>),
    Table(Iter<'a, BoundAclEntry<TablePrivileges>>),
}
enum Columns<'a> {
    None,
    System(BudgetedMapIter<'a, String, SystemAcl>),
    Graph(BudgetedMapIter<'a, String, u32>),
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
    columns: Columns<'a>,
    tail: Tail<'a>,
    column_type: Option<&'a ColumnType>,
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
            columns: Columns::None,
            tail: Tail::None,
            column_type: None,
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
            Acl::Object(acl) => {
                if let Some(entry) = acl.next() {
                    self.fields.object_acl(entry);
                    return true;
                }
            }
            Acl::Table(acl) => {
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
    fn system_acl(&mut self, value: &'a SystemAcl) {
        let SystemAcl { revision, acl } = value;
        self.fields.scalar(*revision);
        self.fields.count(acl.len());
        self.acl = Acl::Table(acl.iter());
    }
    fn next_column(&mut self) -> bool {
        match &mut self.columns {
            Columns::System(columns) => {
                if let Some((name, value)) = columns.next() {
                    self.fields.text(name);
                    self.system_acl(value);
                    return true;
                }
            }
            Columns::Graph(columns) => {
                if let Some((name, oid)) = columns.next() {
                    self.fields.text(name);
                    self.fields.scalar(oid.to_le_bytes());
                    return true;
                }
            }
            Columns::None => {}
        }
        self.columns = Columns::None;
        false
    }
    fn next_child(&mut self) -> bool {
        if self.next_acl() {
            return true;
        }
        if let Some(value) = self.column_type.take() {
            self.column_type = self.fields.column_type(value);
            return true;
        }
        if self.next_column() {
            return true;
        }
        if self
            .tail
            .next_fields(&mut self.fields, &mut self.columns, &mut self.column_type)
        {
            return true;
        }
        false
    }
    fn next_record(&mut self) -> bool {
        self.fields = Fields::new();
        if self.next_child() {
            return true;
        }
        match &mut self.entries {
            Entries::Roles(entries) => {
                let Some((key, role)) = entries.next() else {
                    return false;
                };
                self.fields.role(key, role);
            }
            Entries::Memberships(entries) => {
                let Some((key, membership)) = entries.next() else {
                    return false;
                };
                self.fields.membership(key, membership);
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
            Entries::BuiltinRoutines(entries) => {
                let Some((oid, value)) = entries.next() else {
                    return false;
                };
                let BuiltinRoutineSecurity {
                    revision,
                    execute_acl,
                } = value;
                self.fields.scalar(oid.to_le_bytes());
                self.fields.scalar(*revision);
                self.fields.count(execute_acl.len());
                self.acl = Acl::Object(execute_acl.iter());
            }
            Entries::SystemRelations(entries) => {
                let Some((key, value)) = entries.next() else {
                    return false;
                };
                let RelationIdentity { schema, name } = key;
                let SystemRelationSecurity { table, columns } = value;
                self.fields.text(schema);
                self.fields.text(name);
                self.fields.count(columns.len());
                self.fields.flag(table.is_some());
                self.columns = Columns::System(columns.iter());
                if let Some(table) = table {
                    self.system_acl(table);
                }
            }
            Entries::Composites(_)
            | Entries::Ranges(_)
            | Entries::Enums(_)
            | Entries::Shells(_)
            | Entries::ForeignServers(_)
            | Entries::GraphOids(_) => return self.next_metadata(),
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
