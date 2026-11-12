//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native hierarchy construction keeps original order, provenance and bound scalar ownership.

use super::{invalid, payload, Fields, ImageValue, Input};
use crate::{
    ast::{
        Expr, ForeignKey, PartitionBound, PartitionIdentityOverride, PartitionRangeDatum,
        PartitionSpec, PartitionStrategy, TableHierarchy, TableKeyConstraint,
    },
    SQLError,
};
use std::io::Read;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation},
    CancellationToken,
};

/// Reconstruct one private table hierarchy; enclosing restore validates relation membership and partition compatibility.
pub fn read_table_hierarchy_image(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<TableHierarchy>, SQLError> {
    Ok(read_table_hierarchy_image_with_footprint(reader, memory, cancellation)?.0)
}

/// Reconstruct native hierarchy metadata and report the independently retained scalar payloads. Their constructor leases remain attached to the decoded literals.
pub fn read_table_hierarchy_image_with_footprint(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(Budgeted<TableHierarchy>, usize), SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    if input.header(*b"UQATHR01")? != 1 {
        return Err(invalid("table hierarchy count"));
    }
    let mut reservation = memory.empty_reservation();
    let hierarchy = input.table_hierarchy(&mut reservation)?;
    input.binary.end()?;
    Ok((
        Budgeted::new(hierarchy, reservation),
        input.retained_children,
    ))
}

/// Resume the native hierarchy with its original scalar payload leases and retained footprint.
pub async fn read_table_hierarchy_image_from(
    binary: AsyncImageInput<'_>,
) -> Result<(Budgeted<TableHierarchy>, usize), SQLError> {
    let mut input = Fields {
        binary,
        retained_children: &mut 0,
    };
    if input.header(*b"UQATHR01").await? != 1 {
        return Err(invalid("table hierarchy count"));
    }
    let mut reservation = input.binary.memory.empty_reservation();
    let hierarchy = await_field!(input.table_hierarchy(&mut reservation))?;
    input.binary.end().await?;
    Ok((
        Budgeted::new(hierarchy, reservation),
        *input.retained_children,
    ))
}

// Both drivers consume the same native fields. The synchronous driver retains only
// the currently active field future on its caller's stack.
macro_rules! hierarchy_fields {
    ($input:expr, $memory:expr, $read:ident, $values:ident, $partition:ident) => {{
        let parent_count = $read!($input.binary.count())?;
        let sequence_count = $read!($input.binary.count())?;
        let has_spec = $read!($input.flag())?;
        let has_bound = $read!($input.flag())?;
        let local_count = $read!($input.binary.count())?;
        let override_count = $read!($input.binary.count())?;
        let key_count = $read!($input.binary.count())?;
        let foreign_count = $read!($input.binary.count())?;
        let parents = $values!($input, String, parent_count, $memory)?;
        let parent_sequence_numbers = $values!($input, i32, sequence_count, $memory)?;
        let partition_spec = if has_spec {
            Some($partition!($input, partition_spec, $memory)?)
        } else {
            None
        };
        let partition_bound = if has_bound {
            Some($partition!($input, partition_bound, $memory)?)
        } else {
            None
        };
        let local_columns = $values!($input, String, local_count, $memory)?;
        let partition_identity_overrides =
            $values!($input, PartitionIdentityOverride, override_count, $memory)?;
        let partition_inherited_key_constraints =
            $values!($input, TableKeyConstraint, key_count, $memory)?;
        let partition_inherited_foreign_keys =
            $values!($input, ForeignKey, foreign_count, $memory)?;
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
    }};
}

macro_rules! read_hierarchy_values {
    ($input:expr, $ty:ty, $count:expr, $memory:expr) => {
        $input.image_values::<$ty>($count, $memory)
    };
}
macro_rules! await_hierarchy_values {
    ($input:expr, $ty:ty, $count:expr, $memory:expr) => {
        Ok::<_, SQLError>(payload(
            await_field!($input.image_values::<$ty>($count))?,
            $memory,
        ))
    };
}
macro_rules! read_hierarchy_partition {
    ($input:expr, $method:ident, $memory:expr) => {
        $input.fields(async |mut fields| fields.$method($memory).await)
    };
}
macro_rules! await_hierarchy_partition {
    ($input:expr, $method:ident, $memory:expr) => {
        await_field!($input.$method($memory))
    };
}

impl Input<'_> {
    fn table_hierarchy(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<TableHierarchy, SQLError> {
        hierarchy_fields!(
            self,
            memory,
            read_field,
            read_hierarchy_values,
            read_hierarchy_partition
        )
    }
}

impl Fields<'_> {
    async fn table_hierarchy(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<TableHierarchy, SQLError> {
        hierarchy_fields!(
            self,
            memory,
            await_field,
            await_hierarchy_values,
            await_hierarchy_partition
        )
    }

    pub(super) async fn partition_spec(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<PartitionSpec, SQLError> {
        let strategy = match self.binary.array::<1>().await?[0] {
            0 => PartitionStrategy::List,
            1 => PartitionStrategy::Range,
            2 => PartitionStrategy::Hash,
            _ => return Err(invalid("partition strategy")),
        };
        let count = self.binary.count().await?;
        let keys = self.expressions(count, memory).await?;
        Ok(PartitionSpec { strategy, keys })
    }

    async fn expressions(
        &mut self,
        count: usize,
        memory: &mut MemoryReservation,
    ) -> Result<Vec<Expr>, SQLError> {
        let mut values = BudgetedVec::new(self.binary.memory);
        values.reserve(count)?;
        for _ in 0..count {
            self.binary.checkpoint().await?;
            values.push(payload(self.expression().await?, memory))?;
        }
        let (values, reservation) = values.into_parts();
        memory.absorb(reservation);
        Ok(values)
    }

    async fn range_datum(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<PartitionRangeDatum, SQLError> {
        match self.binary.array::<1>().await?[0] {
            0 => Ok(PartitionRangeDatum::MinValue),
            1 => Ok(PartitionRangeDatum::Value(payload(
                self.expression().await?,
                memory,
            ))),
            2 => Ok(PartitionRangeDatum::MaxValue),
            _ => Err(invalid("partition range datum")),
        }
    }

    pub(super) async fn partition_bound(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<PartitionBound, SQLError> {
        match self.binary.array::<1>().await?[0] {
            0 => Ok(PartitionBound::Default),
            1 => {
                let count = self.binary.count().await?;
                Ok(PartitionBound::List(self.expressions(count, memory).await?))
            }
            2 => {
                let lower_count = self.binary.count().await?;
                let upper_count = self.binary.count().await?;
                Ok(PartitionBound::Range {
                    lower: payload(
                        self.image_values::<PartitionRangeDatum>(lower_count)
                            .await?,
                        memory,
                    ),
                    upper: payload(
                        self.image_values::<PartitionRangeDatum>(upper_count)
                            .await?,
                        memory,
                    ),
                })
            }
            3 => Ok(PartitionBound::Hash {
                modulus: i32::from_le_bytes(self.binary.array().await?),
                remainder: i32::from_le_bytes(self.binary.array().await?),
            }),
            _ => Err(invalid("partition bound kind")),
        }
    }
}

impl ImageValue for i32 {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        Ok(Budgeted::new(
            i32::from_le_bytes(input.binary.array().await?),
            input.binary.memory.empty_reservation(),
        ))
    }
}

impl ImageValue for PartitionRangeDatum {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        let mut memory = input.binary.memory.empty_reservation();
        let value = input.range_datum(&mut memory).await?;
        Ok(Budgeted::new(value, memory))
    }
}

impl ImageValue for PartitionIdentityOverride {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        let mut memory = input.binary.memory.empty_reservation();
        let column = payload(input.binary.text().await?, &mut memory);
        let original = if input.flag().await? {
            Some(input.auto_increment(&mut memory).await?)
        } else {
            None
        };
        Ok(Budgeted::new(Self { column, original }, memory))
    }
}
