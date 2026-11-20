//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed comparison kernels shared by declaration admission and captured bounds.

use crate::{ast::PartitionRangeDatum, expr::SQLValueCatalog, ColumnType, SQLError};
use std::cmp::Ordering;
use uqa_core::memory::ProductionControl;

pub(super) fn range_points(
    left: &[PartitionRangeDatum],
    right: &[PartitionRangeDatum],
    types: &[ColumnType],
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Ordering, SQLError> {
    crate::catalog::work::inline(range_points_with_work(
        left,
        right,
        types,
        &|| catalog,
        control,
        &mut (),
    ))
}

pub(super) async fn range_points_with_work<'a>(
    left: &[PartitionRangeDatum],
    right: &[PartitionRangeDatum],
    types: &[ColumnType],
    catalog: &impl Fn() -> Option<&'a dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
    work: &mut (impl crate::catalog::CatalogWork + Send),
) -> Result<Ordering, SQLError> {
    control.check()?;
    if left.len() != right.len() || left.len() != types.len() {
        return Err(SQLError::Internal(
            "partition range points have different widths".into(),
        ));
    }
    for ((left, right), ty) in left.iter().zip(right).zip(types) {
        work.checkpoint().await?;
        control.check()?;
        let ordering = match (left, right) {
            (PartitionRangeDatum::MinValue, PartitionRangeDatum::MinValue)
            | (PartitionRangeDatum::MaxValue, PartitionRangeDatum::MaxValue) => Ordering::Equal,
            (PartitionRangeDatum::MinValue, _) | (_, PartitionRangeDatum::MaxValue) => {
                Ordering::Less
            }
            (PartitionRangeDatum::MaxValue, _) | (_, PartitionRangeDatum::MinValue) => {
                Ordering::Greater
            }
            (PartitionRangeDatum::Value(left), PartitionRangeDatum::Value(right)) => {
                crate::expr::compare_selected_values_with_catalog(
                    super::stored_datum(left)?,
                    super::stored_datum(right)?,
                    Some(ty),
                    control,
                    catalog(),
                    None,
                )?
            }
        };
        if ordering != Ordering::Equal {
            return Ok(ordering);
        }
    }
    Ok(Ordering::Equal)
}

/// First residue in the new partition's coverage also owned by an existing partition. Positive moduli belong to a divisibility chain; no modulus-sized bitmap or repeated addition is required.
pub(super) fn first_shared_hash_residue(
    modulus: i32,
    remainder: i32,
    existing_modulus: i32,
    existing_remainder: i32,
    greatest: i32,
) -> Option<i32> {
    if modulus <= existing_modulus {
        (existing_remainder % modulus == remainder).then_some(existing_remainder)
    } else {
        let first = remainder % greatest;
        (first % existing_modulus == existing_remainder).then_some(first)
    }
}
