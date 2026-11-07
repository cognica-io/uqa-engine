//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native hierarchy construction keeps original order, provenance and bound scalar ownership.

use super::{invalid, payload, Input};
use crate::{
    ast::{
        Expr, PartitionBound, PartitionIdentityOverride, PartitionRangeDatum, PartitionSpec,
        PartitionStrategy, TableHierarchy,
    },
    SQLError,
};
use std::io::Read;
use uqa_core::{
    memory::{Budgeted, MemoryBudget, MemoryReservation},
    CancellationToken,
};

/// Reconstruct one private table hierarchy; enclosing restore validates relation membership and partition compatibility.
pub fn read_table_hierarchy_image(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<TableHierarchy>, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    if input.header(*b"UQATHR01")? != 1 {
        return Err(invalid("table hierarchy count"));
    }
    let mut reservation = memory.empty_reservation();
    let hierarchy = input.table_hierarchy(&mut reservation)?;
    input.binary.end()?;
    Ok(Budgeted::new(hierarchy, reservation))
}

impl Input<'_> {
    fn table_hierarchy(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<TableHierarchy, SQLError> {
        let parent_count = self.binary.count()?;
        let sequence_count = self.binary.count()?;
        let has_spec = self.flag()?;
        let has_bound = self.flag()?;
        let local_count = self.binary.count()?;
        let override_count = self.binary.count()?;
        let key_count = self.binary.count()?;
        let foreign_count = self.binary.count()?;
        let parents = self.texts(parent_count, memory)?;
        let parent_sequence_numbers = self.items(sequence_count, memory, |input, _| {
            Ok(i32::from_le_bytes(input.binary.array()?))
        })?;
        let partition_spec = has_spec.then(|| self.partition_spec(memory)).transpose()?;
        let partition_bound = has_bound
            .then(|| self.partition_bound(memory))
            .transpose()?;
        let local_columns = self.texts(local_count, memory)?;
        let partition_identity_overrides =
            self.items(override_count, memory, |input, memory| {
                let column = payload(input.binary.text()?, memory);
                let original = input
                    .flag()?
                    .then(|| input.auto_increment(memory))
                    .transpose()?;
                Ok(PartitionIdentityOverride { column, original })
            })?;
        let partition_inherited_key_constraints = self.items(key_count, memory, Self::table_key)?;
        let partition_inherited_foreign_keys =
            self.items(foreign_count, memory, Self::table_foreign_key)?;
        Ok(TableHierarchy {
            parents,
            parent_sequence_numbers,
            partition_spec,
            partition_bound,
            local_columns,
            partition_identity_overrides,
            partition_inherited_key_constraints,
            partition_inherited_foreign_keys,
        })
    }

    pub(super) fn partition_spec(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<PartitionSpec, SQLError> {
        let strategy = match self.binary.array::<1>()?[0] {
            0 => PartitionStrategy::List,
            1 => PartitionStrategy::Range,
            2 => PartitionStrategy::Hash,
            _ => return Err(invalid("partition strategy")),
        };
        let count = self.binary.count()?;
        let keys = self.expressions(count, memory)?;
        Ok(PartitionSpec { strategy, keys })
    }

    fn expressions(
        &mut self,
        count: usize,
        memory: &mut MemoryReservation,
    ) -> Result<Vec<Expr>, SQLError> {
        self.items(count, memory, |input, memory| {
            Ok(payload(input.expression()?, memory))
        })
    }

    fn range_datum(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<PartitionRangeDatum, SQLError> {
        match self.binary.array::<1>()?[0] {
            0 => Ok(PartitionRangeDatum::MinValue),
            1 => Ok(PartitionRangeDatum::Value(payload(
                self.expression()?,
                memory,
            ))),
            2 => Ok(PartitionRangeDatum::MaxValue),
            _ => Err(invalid("partition range datum")),
        }
    }

    pub(super) fn partition_bound(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<PartitionBound, SQLError> {
        match self.binary.array::<1>()?[0] {
            0 => Ok(PartitionBound::Default),
            1 => {
                let count = self.binary.count()?;
                Ok(PartitionBound::List(self.expressions(count, memory)?))
            }
            2 => {
                let lower_count = self.binary.count()?;
                let upper_count = self.binary.count()?;
                Ok(PartitionBound::Range {
                    lower: self.items(lower_count, memory, Self::range_datum)?,
                    upper: self.items(upper_count, memory, Self::range_datum)?,
                })
            }
            3 => Ok(PartitionBound::Hash {
                modulus: i32::from_le_bytes(self.binary.array()?),
                remainder: i32::from_le_bytes(self.binary.array()?),
            }),
            _ => Err(invalid("partition bound kind")),
        }
    }
}
