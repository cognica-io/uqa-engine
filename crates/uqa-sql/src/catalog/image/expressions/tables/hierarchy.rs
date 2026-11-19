//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Hierarchy images preserve written parent order and already bound partition syntax.

use super::{constraints, DefinitionImageReader, Frame, Part as TablePart};
use crate::{
    ast::{
        PartitionBound, PartitionIdentityOverride, PartitionRangeDatum, PartitionSpec,
        PartitionStrategy, TableHierarchy,
    },
    SQLError,
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

pub(in super::super) enum Part<'a> {
    Definition(&'a TableHierarchy),
    Spec(&'a PartitionSpec),
    Bound(&'a PartitionBound),
    Range(&'a [PartitionRangeDatum]),
    SequenceNumbers(&'a [i32]),
    Overrides(&'a [PartitionIdentityOverride]),
}

/// Borrow native inheritance and partition metadata without resolving parents or reevaluating bounds.
pub fn table_hierarchy_image_reader<'a>(
    value: &'a TableHierarchy,
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
) -> Result<DefinitionImageReader<'a>, SQLError> {
    DefinitionImageReader::start(
        Frame::Table(TablePart::Hierarchy(Part::Definition(value))),
        *b"UQATHR01",
        1,
        memory,
        cancellation,
    )
}

impl<'a> DefinitionImageReader<'a> {
    pub(super) fn table_hierarchy_part(&mut self, part: Part<'a>) -> Result<(), SQLError> {
        match part {
            Part::Definition(value) => self.table_hierarchy(value)?,
            Part::Spec(PartitionSpec { strategy, keys }) => {
                self.fields.scalar([match strategy {
                    PartitionStrategy::List => 0,
                    PartitionStrategy::Range => 1,
                    PartitionStrategy::Hash => 2,
                }]);
                self.fields.count(keys.len());
                self.push(Frame::Nodes(keys))?;
            }
            Part::Bound(value) => self.partition_bound(value)?,
            Part::Range(values) => {
                if let Some((first, rest)) = values.split_first() {
                    self.table_push(TablePart::Hierarchy(Part::Range(rest)))?;
                    match first {
                        PartitionRangeDatum::MinValue => self.fields.scalar([0]),
                        PartitionRangeDatum::Value(value) => {
                            self.fields.scalar([1]);
                            self.push(Frame::Node(value))?;
                        }
                        PartitionRangeDatum::MaxValue => self.fields.scalar([2]),
                    }
                }
            }
            Part::SequenceNumbers(values) => {
                if let Some((first, rest)) = values.split_first() {
                    self.fields.scalar(first.to_le_bytes());
                    self.table_push(TablePart::Hierarchy(Part::SequenceNumbers(rest)))?;
                }
            }
            Part::Overrides(values) => {
                if let Some((PartitionIdentityOverride { column, original }, rest)) =
                    values.split_first()
                {
                    self.fields.text(column);
                    self.fields.flag(original.is_some());
                    self.table_push(TablePart::Hierarchy(Part::Overrides(rest)))?;
                    if let Some(original) = original {
                        self.table_push(TablePart::AutoIncrement(original))?;
                    }
                }
            }
        }
        Ok(())
    }

    fn table_hierarchy(&mut self, value: &'a TableHierarchy) -> Result<(), SQLError> {
        let TableHierarchy {
            parents,
            parent_sequence_numbers,
            partition_spec,
            partition_bound,
            local_columns,
            partition_identity_overrides,
            partition_inherited_key_constraints,
            partition_inherited_foreign_keys,
        } = value;
        self.fields.count(parents.len());
        self.fields.count(parent_sequence_numbers.len());
        self.fields.flag(partition_spec.is_some());
        self.fields.flag(partition_bound.is_some());
        self.fields.count(local_columns.len());
        self.fields.count(partition_identity_overrides.len());
        self.fields.count(partition_inherited_key_constraints.len());
        self.fields.count(partition_inherited_foreign_keys.len());
        self.table_push(TablePart::Constraints(constraints::Part::ForeignKeys(
            partition_inherited_foreign_keys,
        )))?;
        self.table_push(TablePart::Constraints(constraints::Part::Keys(
            partition_inherited_key_constraints,
        )))?;
        self.table_push(TablePart::Hierarchy(Part::Overrides(
            partition_identity_overrides,
        )))?;
        self.table_push(TablePart::Texts(local_columns))?;
        if let Some(bound) = partition_bound {
            self.table_push(TablePart::Hierarchy(Part::Bound(bound)))?;
        }
        if let Some(spec) = partition_spec {
            self.table_push(TablePart::Hierarchy(Part::Spec(spec)))?;
        }
        self.table_push(TablePart::Hierarchy(Part::SequenceNumbers(
            parent_sequence_numbers,
        )))?;
        self.table_push(TablePart::Texts(parents))
    }

    fn partition_bound(&mut self, value: &'a PartitionBound) -> Result<(), SQLError> {
        match value {
            PartitionBound::Default => self.fields.scalar([0]),
            PartitionBound::List(values) => {
                self.fields.scalar([1]);
                self.fields.count(values.len());
                self.push(Frame::Nodes(values))?;
            }
            PartitionBound::Range { lower, upper } => {
                self.fields.scalar([2]);
                self.fields.count(lower.len());
                self.fields.count(upper.len());
                self.table_push(TablePart::Hierarchy(Part::Range(upper)))?;
                self.table_push(TablePart::Hierarchy(Part::Range(lower)))?;
            }
            PartitionBound::Hash { modulus, remainder } => {
                self.fields.scalar([3]);
                self.fields.scalar(modulus.to_le_bytes());
                self.fields.scalar(remainder.to_le_bytes());
            }
        }
        Ok(())
    }
}
