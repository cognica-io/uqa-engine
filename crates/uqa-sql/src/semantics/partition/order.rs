//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Partitions in the order of `PostgreSQL`'s partition descriptor, which numbers the objects derived on a partitioned table's partitions, such as the constraints a foreign key derives on the partitions it references.

use super::PartitionContext;
use crate::{
    ast::{PartitionBound, PartitionRangeDatum},
    SQLError,
};
use uqa_core::Value;

#[cfg(test)]
mod tests;

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum RangePoint {
    MinValue,
    Value(Value),
    MaxValue,
}

/// A partition's place among its siblings, which share a partitioning strategy apart from the default partition.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Position {
    /// A range partition by its lower bound.
    Range(Vec<RangePoint>),
    /// A list partition by its smallest value.
    List(Value),
    /// A hash partition by modulus, then remainder.
    Hash(i32, i32),
    /// A list partition that holds only NULL follows every list partition that holds a value.
    NullList,
    Default,
}

fn position(context: &PartitionContext<'_>, bound: &PartitionBound) -> Result<Position, SQLError> {
    let evaluate = |expression| context.expressions.evaluate_bound(expression, &[]);
    Ok(match bound {
        PartitionBound::Default => Position::Default,
        PartitionBound::Range { lower, .. } => Position::Range(
            lower
                .iter()
                .map(|datum| {
                    Ok(match datum {
                        PartitionRangeDatum::MinValue => RangePoint::MinValue,
                        PartitionRangeDatum::Value(expression) => {
                            RangePoint::Value(evaluate(expression)?)
                        }
                        PartitionRangeDatum::MaxValue => RangePoint::MaxValue,
                    })
                })
                .collect::<Result<_, SQLError>>()?,
        ),
        PartitionBound::List(values) => {
            let mut smallest = None;
            for value in values {
                let value = evaluate(value)?;
                if value != Value::Null && smallest.as_ref().is_none_or(|current| value < *current)
                {
                    smallest = Some(value);
                }
            }
            smallest.map_or(Position::NullList, Position::List)
        }
        PartitionBound::Hash { modulus, remainder } => Position::Hash(*modulus, *remainder),
    })
}

/// The names of `partitions`, which are siblings, in the order of `PostgreSQL`'s partition descriptor: range partitions by lower bound, list partitions by their smallest value followed by a partition that holds only NULL, hash partitions by modulus and remainder, and the default partition last.
pub fn partition_bound_order(
    context: &PartitionContext<'_>,
    partitions: Vec<(String, PartitionBound)>,
) -> Result<Vec<String>, SQLError> {
    let mut positioned = partitions
        .into_iter()
        .map(|(name, bound)| Ok((position(context, &bound)?, name)))
        .collect::<Result<Vec<_>, SQLError>>()?;
    positioned.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(positioned.into_iter().map(|(_, name)| name).collect())
}
