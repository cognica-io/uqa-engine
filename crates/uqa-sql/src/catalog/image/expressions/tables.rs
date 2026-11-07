//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table declarations borrow original physical attributes, constraints and evaluated defaults.

use super::{DefinitionImageReader, Frame};
use crate::{
    ast::{
        AutoIncrement, ColumnDef, DeferredSQLError, ForeignKeyAction, ForeignKeyMatch,
        ForeignKeyRef, FunctionBinding, GeneratedColumn, GeneratedColumnKind,
        IdentitySequenceDeclaration, ReferencedPartitionConstraint, SequenceDeclaration,
    },
    SQLError,
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

mod sequences;
#[cfg(test)]
mod tests;

pub(super) enum Part<'a> {
    Columns(&'a [ColumnDef]),
    Column(&'a ColumnDef),
    ColumnConstraints(&'a ColumnDef),
    ColumnValues(&'a ColumnDef),
    Generated(&'a GeneratedColumn),
    Bindings(&'a [FunctionBinding]),
    AutoIncrement(&'a AutoIncrement),
    IdentityDeclaration(&'a IdentitySequenceDeclaration),
    SequenceDeclaration(&'a SequenceDeclaration),
    DeferredError(&'a DeferredSQLError),
    Texts(&'a [String]),
    ForeignReference(&'a ForeignKeyRef),
    ReferencedPartitions(&'a [ReferencedPartitionConstraint]),
}

/// Borrow ordered native columns, including original type, default, generated and constraint metadata. Preparation does not scan columns; each read emits at most 16 KiB under the supplied scratch allowance.
pub fn columns_image_reader<'a>(
    columns: &'a [ColumnDef],
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
) -> Result<DefinitionImageReader<'a>, SQLError> {
    DefinitionImageReader::start(
        Frame::Table(Part::Columns(columns)),
        *b"UQACOL01",
        columns.len(),
        memory,
        cancellation,
    )
}

impl<'a> DefinitionImageReader<'a> {
    fn table_push(&mut self, part: Part<'a>) -> Result<(), SQLError> {
        self.push(Frame::Table(part))
    }

    pub(super) fn table_part(&mut self, part: Part<'a>) -> Result<(), SQLError> {
        match part {
            Part::Columns(values) => {
                if let Some((first, rest)) = values.split_first() {
                    self.table_push(Part::Columns(rest))?;
                    self.table_push(Part::Column(first))?;
                }
            }
            Part::Column(value) => self.column_header(value)?,
            Part::ColumnConstraints(value) => {
                self.fields.optional_text(value.not_null_name.as_deref());
                self.constraint_identity(value.not_null_identity);
                self.fields.optional_text(value.check_name.as_deref());
                self.fields.optional_incarnation(value.check_object_id);
                for oid in [value.check_catalog_oid, value.default_catalog_oid] {
                    self.fields.flag(oid.is_some());
                    if let Some(oid) = oid {
                        self.fields.scalar(oid.to_le_bytes());
                    }
                }
                self.table_push(Part::ColumnValues(value))?;
            }
            Part::ColumnValues(value) => {
                if let Some(reference) = &value.references {
                    self.table_push(Part::ForeignReference(reference))?;
                }
                self.optional_node(value.check.as_ref())?;
                if let Some(generated) = &value.generated {
                    self.table_push(Part::Generated(generated))?;
                }
                self.optional_node(value.default.as_ref())?;
                if let Some(auto) = &value.auto_increment {
                    self.table_push(Part::AutoIncrement(auto))?;
                }
                if let Some(value) = &value.missing_value {
                    self.push(Frame::Value(value))?;
                }
            }
            Part::Generated(value) => {
                let GeneratedColumn {
                    kind,
                    expression,
                    function_dependencies,
                } = value;
                self.fields.scalar([match kind {
                    GeneratedColumnKind::Virtual => 0,
                    GeneratedColumnKind::Stored => 1,
                }]);
                self.fields.count(function_dependencies.len());
                self.table_push(Part::Bindings(function_dependencies))?;
                self.push(Frame::Node(expression))?;
            }
            Part::Bindings(values) => {
                if let Some((first, rest)) = values.split_first() {
                    self.table_push(Part::Bindings(rest))?;
                    self.push(Frame::Binding(first))?;
                }
            }
            Part::Texts(values) => {
                if let Some((first, rest)) = values.split_first() {
                    self.fields.text(first);
                    self.table_push(Part::Texts(rest))?;
                }
            }
            Part::ForeignReference(value) => self.foreign_reference(value)?,
            Part::ReferencedPartitions(values) => {
                if let Some((first, rest)) = values.split_first() {
                    let ReferencedPartitionConstraint {
                        partition,
                        parent,
                        name,
                        catalog_identity,
                        validated,
                    } = first;
                    self.fields.scalar(*partition);
                    self.fields.optional_incarnation(*parent);
                    self.fields.text(name);
                    self.fields.scalar(catalog_identity.object_id);
                    self.fields.scalar(catalog_identity.oid.to_le_bytes());
                    self.fields.flag(*validated);
                    self.table_push(Part::ReferencedPartitions(rest))?;
                }
            }
            other => self.sequence_part(other)?,
        }
        Ok(())
    }

    fn column_header(&mut self, value: &'a ColumnDef) -> Result<(), SQLError> {
        let ColumnDef {
            name,
            ty,
            collation_oid,
            object_id,
            attribute_number,
            missing_value,
            primary_key,
            not_null,
            not_null_explicit,
            not_null_name: _,
            not_null_identity: _,
            not_null_validated,
            not_null_no_inherit,
            not_null_is_local,
            auto_increment,
            unique,
            default,
            generated,
            default_catalog_oid: _,
            check,
            check_name: _,
            check_enforced,
            check_validated,
            check_no_inherit,
            check_is_local,
            check_object_id: _,
            check_catalog_oid: _,
            references,
        } = value;
        self.fields.text(name);
        self.fields.optional_u32(*collation_oid);
        self.fields.optional_incarnation(*object_id);
        self.fields.flag(attribute_number.is_some());
        if let Some(number) = attribute_number {
            self.fields.scalar(number.to_le_bytes());
        }
        self.fields.scalar([
            u8::from(*primary_key),
            u8::from(*not_null),
            u8::from(*not_null_explicit),
            u8::from(*not_null_validated),
            u8::from(*not_null_no_inherit),
            u8::from(*not_null_is_local),
            u8::from(*unique),
            u8::from(*check_enforced),
            u8::from(*check_validated),
            u8::from(*check_no_inherit),
            u8::from(*check_is_local),
            u8::from(missing_value.is_some()),
            u8::from(auto_increment.is_some()),
            u8::from(default.is_some()),
            u8::from(generated.is_some()),
            u8::from(check.is_some()),
            u8::from(references.is_some()),
        ]);
        self.table_push(Part::ColumnConstraints(value))?;
        self.push(Frame::Type(ty))?;
        Ok(())
    }

    fn foreign_reference(&mut self, value: &'a ForeignKeyRef) -> Result<(), SQLError> {
        let ForeignKeyRef {
            referenced_index,
            referenced_key,
            name,
            object_id,
            catalog_identity,
            table,
            column,
            on_update,
            on_delete,
            match_type,
            enforced,
            validated,
            deferrable,
            initially_deferred,
            period,
            referenced_partitions,
        } = value;
        self.fields.optional_incarnation(*referenced_index);
        self.fields.optional_text(referenced_key.as_deref());
        self.fields.optional_text(name.as_deref());
        self.fields.optional_incarnation(*object_id);
        self.constraint_identity(*catalog_identity);
        self.fields.text(table);
        self.fields.optional_text(column.as_deref());
        self.fields.scalar([
            foreign_action(*on_update),
            foreign_action(*on_delete),
            match match_type {
                ForeignKeyMatch::Simple => 0,
                ForeignKeyMatch::Full => 1,
            },
            u8::from(*enforced),
            u8::from(*validated),
            u8::from(*deferrable),
            u8::from(*initially_deferred),
            u8::from(*period),
        ]);
        self.fields.count(referenced_partitions.len());
        self.table_push(Part::ReferencedPartitions(referenced_partitions))
    }
}

fn foreign_action(action: ForeignKeyAction) -> u8 {
    match action {
        ForeignKeyAction::NoAction => 0,
        ForeignKeyAction::Restrict => 1,
        ForeignKeyAction::Cascade => 2,
        ForeignKeyAction::SetNull => 3,
        ForeignKeyAction::SetDefault => 4,
    }
}
