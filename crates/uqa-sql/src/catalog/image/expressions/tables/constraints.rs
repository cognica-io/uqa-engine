//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered table constraints retain identities and declaration provenance.

use super::{foreign_action, hierarchy, DefinitionImageReader, Frame, Part as TablePart};
use crate::{
    ast::{ForeignKey, ForeignKeyMatch, TableCheck, TableKeyConstraint, TableKeyConstraintKind},
    SQLError,
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

pub(in super::super) enum Part<'a> {
    Checks(&'a [TableCheck]),
    Keys(&'a [TableKeyConstraint]),
    ForeignKeys(&'a [ForeignKey]),
}

/// Borrow ordered CHECK definitions, including original scalar bindings and detached partition bounds.
pub fn table_checks_image_reader<'a>(
    values: &'a [TableCheck],
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
) -> Result<DefinitionImageReader<'a>, SQLError> {
    DefinitionImageReader::start(
        Frame::Table(TablePart::Constraints(Part::Checks(values))),
        *b"UQATCK01",
        values.len(),
        memory,
        cancellation,
    )
}

/// Borrow ordered key constraints without recreating their supporting indexes or identities.
pub fn table_keys_image_reader<'a>(
    values: &'a [TableKeyConstraint],
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
) -> Result<DefinitionImageReader<'a>, SQLError> {
    DefinitionImageReader::start(
        Frame::Table(TablePart::Constraints(Part::Keys(values))),
        *b"UQATKY01",
        values.len(),
        memory,
        cancellation,
    )
}

/// Borrow ordered foreign keys with their exact referenced index, enforcement family and partition identities.
pub fn foreign_keys_image_reader<'a>(
    values: &'a [ForeignKey],
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
) -> Result<DefinitionImageReader<'a>, SQLError> {
    DefinitionImageReader::start(
        Frame::Table(TablePart::Constraints(Part::ForeignKeys(values))),
        *b"UQAFKY01",
        values.len(),
        memory,
        cancellation,
    )
}

impl<'a> DefinitionImageReader<'a> {
    pub(super) fn table_constraint_part(&mut self, part: Part<'a>) -> Result<(), SQLError> {
        match part {
            Part::Checks(values) => {
                if let Some((first, rest)) = values.split_first() {
                    self.table_push(TablePart::Constraints(Part::Checks(rest)))?;
                    self.table_check(first)?;
                }
            }
            Part::Keys(values) => {
                if let Some((first, rest)) = values.split_first() {
                    self.table_push(TablePart::Constraints(Part::Keys(rest)))?;
                    self.table_key(first)?;
                }
            }
            Part::ForeignKeys(values) => {
                if let Some((first, rest)) = values.split_first() {
                    self.table_push(TablePart::Constraints(Part::ForeignKeys(rest)))?;
                    self.table_foreign_key(first)?;
                }
            }
        }
        Ok(())
    }

    fn table_check(&mut self, value: &'a TableCheck) -> Result<(), SQLError> {
        let TableCheck {
            name,
            object_id,
            catalog_oid,
            is_local,
            expr,
            enforced,
            validated,
            no_inherit,
            partition_constraint,
        } = value;
        self.fields.optional_text(name.as_deref());
        self.fields.optional_incarnation(*object_id);
        self.fields.flag(catalog_oid.is_some());
        if let Some(oid) = catalog_oid {
            self.fields.scalar(oid.to_le_bytes());
        }
        self.fields.scalar([
            u8::from(*is_local),
            u8::from(*enforced),
            u8::from(*validated),
            u8::from(*no_inherit),
            u8::from(partition_constraint.is_some()),
        ]);
        if let Some(crate::ast::DetachedPartitionConstraint { spec, bound }) = partition_constraint
        {
            self.table_push(TablePart::Hierarchy(hierarchy::Part::Bound(bound)))?;
            self.table_push(TablePart::Hierarchy(hierarchy::Part::Spec(spec)))?;
        }
        self.push(Frame::Node(expr))
    }

    fn table_key(&mut self, value: &'a TableKeyConstraint) -> Result<(), SQLError> {
        let TableKeyConstraint {
            catalog_identity,
            index_identity,
            name,
            kind,
            columns,
            included_columns,
            nulls_not_distinct,
            without_overlaps,
        } = value;
        self.constraint_identity(*catalog_identity);
        self.constraint_identity(*index_identity);
        self.fields.optional_text(name.as_deref());
        self.fields.scalar([
            match kind {
                TableKeyConstraintKind::PrimaryKey => 0,
                TableKeyConstraintKind::Unique => 1,
            },
            u8::from(*nulls_not_distinct),
            u8::from(*without_overlaps),
        ]);
        self.fields.count(columns.len());
        self.fields.count(included_columns.len());
        self.table_push(TablePart::Texts(included_columns))?;
        self.table_push(TablePart::Texts(columns))
    }

    fn table_foreign_key(&mut self, value: &'a ForeignKey) -> Result<(), SQLError> {
        let ForeignKey {
            referenced_index,
            referenced_key,
            name,
            object_id,
            catalog_identity,
            local_columns,
            ref_table,
            ref_columns,
            on_update,
            on_delete,
            on_delete_set_columns,
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
        self.fields.text(ref_table);
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
        self.fields.count(local_columns.len());
        self.fields.count(ref_columns.len());
        self.fields.count(on_delete_set_columns.len());
        self.fields.count(referenced_partitions.len());
        self.table_push(TablePart::ReferencedPartitions(referenced_partitions))?;
        self.table_push(TablePart::Texts(on_delete_set_columns))?;
        self.table_push(TablePart::Texts(ref_columns))?;
        self.table_push(TablePart::Texts(local_columns))
    }
}
