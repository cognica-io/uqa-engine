//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` admission of a transformed partition bound (`check_new_partition_bound`): a second default partition, an empty range, a broken hash modulus chain, and overlap with an existing partition, each reported against the partitions `PostgreSQL` names.

use super::bounds::parent_partition_key;
use super::datum_text::{range_bound_text, stored_datum};
use super::PartitionContext;
use crate::ast::{PartitionBound, PartitionRangeDatum};
use crate::expr::EngineHook;
use crate::{ColumnType, SQLError};
use std::cmp::Ordering;

/// One existing partition of the parent with its stored bound.
struct Sibling {
    name: String,
    bound: PartitionBound,
}

/// Validate that `bound`, already transformed, can be added for `partition` under `parent`.
pub fn validate_new_partition_bound(
    context: &PartitionContext<'_>,
    parent: &str,
    partition: &str,
    bound: &PartitionBound,
) -> Result<(), SQLError> {
    let (_, keys) = parent_partition_key(context, parent)?;
    let types = keys.into_iter().map(|key| key.ty).collect::<Vec<_>>();
    let siblings = sibling_bounds(context, parent)?;
    let partition = local_relation_name(partition)?;
    let overlapping = match bound {
        PartitionBound::Default => {
            if let Some(default) = siblings
                .iter()
                .find(|sibling| matches!(sibling.bound, PartitionBound::Default))
            {
                return Err(invalid_object_definition(format!(
                    "partition \"{partition}\" conflicts with existing default partition \"{}\"",
                    default.name
                )));
            }
            None
        }
        PartitionBound::List(values) => list_overlap(context, values, &siblings, &types[0])?,
        PartitionBound::Range { lower, upper } => {
            if compare_range_points(context, lower, upper, &types)? != Ordering::Less {
                let engine: &dyn EngineHook = context.assignment;
                return Err(SQLError::Diagnostic {
                    sqlstate: "42P17".into(),
                    message: format!("empty range bound specified for partition \"{partition}\""),
                    detail: Some(format!(
                        "Specified lower bound {} is greater than or equal to upper bound {}.",
                        range_bound_text(lower, &types, Some(engine))?,
                        range_bound_text(upper, &types, Some(engine))?
                    )),
                    hint: None,
                });
            }
            range_overlap(context, lower, upper, &siblings, &types)?
        }
        PartitionBound::Hash { modulus, remainder } => {
            hash_overlap(*modulus, *remainder, &siblings)?
        }
    };
    match overlapping {
        Some(sibling) => Err(invalid_object_definition(format!(
            "partition \"{partition}\" would overlap partition \"{sibling}\""
        ))),
        None => Ok(()),
    }
}

fn sibling_bounds(context: &PartitionContext<'_>, parent: &str) -> Result<Vec<Sibling>, SQLError> {
    let mut siblings = Vec::new();
    for child in context.catalog.direct_hierarchy_children(parent)? {
        let hierarchy = context
            .catalog
            .try_table_hierarchy(&child)
            .map_err(|error| SQLError::Internal(format!("read sibling partition: {error}")))?;
        if let Some(bound) = hierarchy.partition_bound {
            siblings.push(Sibling {
                name: local_relation_name(&child)?,
                bound,
            });
        }
    }
    Ok(siblings)
}

/// The first value of the new list, in declared order, that an existing partition accepts names that partition; NULL overlaps the partition that accepts NULL.
fn list_overlap(
    context: &PartitionContext<'_>,
    values: &[crate::ast::Expr],
    siblings: &[Sibling],
    ty: &ColumnType,
) -> Result<Option<String>, SQLError> {
    for value in values {
        let value = stored_datum(value)?;
        for sibling in siblings {
            let PartitionBound::List(existing) = &sibling.bound else {
                continue;
            };
            for candidate in existing {
                if super::compare_values(context, stored_datum(candidate)?, value, ty)?.is_eq() {
                    return Ok(Some(sibling.name.clone()));
                }
            }
        }
    }
    Ok(None)
}

/// The partition containing the new lower bound, or else the next partition when the new range does not fit in the gap before it: the first overlapping partition in bound order.
fn range_overlap(
    context: &PartitionContext<'_>,
    lower: &[PartitionRangeDatum],
    upper: &[PartitionRangeDatum],
    siblings: &[Sibling],
    types: &[ColumnType],
) -> Result<Option<String>, SQLError> {
    let mut ranges = Vec::new();
    for sibling in siblings {
        if let PartitionBound::Range {
            lower: existing_lower,
            upper: existing_upper,
        } = &sibling.bound
        {
            ranges.push((existing_lower, existing_upper, &sibling.name));
        }
    }
    uqa_core::ordering::sort_by_with_control(
        &mut ranges,
        &mut || Ok::<_, SQLError>(()),
        |left, right, _| compare_range_points(context, left.0, right.0, types),
    )?;
    for (existing_lower, existing_upper, name) in ranges {
        if compare_range_points(context, lower, existing_upper, types)? == Ordering::Less
            && compare_range_points(context, existing_lower, upper, types)? == Ordering::Less
        {
            return Ok(Some(name.clone()));
        }
    }
    Ok(None)
}

/// Compare two range bound points datum by datum, where `MINVALUE` precedes and `MAXVALUE` follows every value.
pub(super) fn compare_range_points(
    context: &PartitionContext<'_>,
    left: &[PartitionRangeDatum],
    right: &[PartitionRangeDatum],
    types: &[ColumnType],
) -> Result<Ordering, SQLError> {
    super::comparison::range_points(
        left,
        right,
        types,
        Some(&crate::expr::EngineValueCatalog(context.assignment)),
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

/// Check the modulus chain against the neighboring existing moduli, then report the first existing partition that covers a remainder the new partition would own.
fn hash_overlap(
    modulus: i32,
    remainder: i32,
    siblings: &[Sibling],
) -> Result<Option<String>, SQLError> {
    let mut existing = siblings
        .iter()
        .filter_map(|sibling| match sibling.bound {
            PartitionBound::Hash { modulus, remainder } => {
                Some((modulus, remainder, &sibling.name))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if existing.is_empty() {
        return Ok(None);
    }
    existing.sort_by_key(|(modulus, remainder, _)| (*modulus, *remainder));
    let not_a_factor = |next: i32, name: &str| {
        modulus_error(format!(
            "The new modulus {modulus} is not a factor of {next}, the modulus of existing partition \"{name}\"."
        ))
    };
    match existing
        .iter()
        .rposition(|(existing_modulus, existing_remainder, _)| {
            (*existing_modulus, *existing_remainder) <= (modulus, remainder)
        }) {
        None => {
            let (next, _, name) = existing[0];
            if next % modulus != 0 {
                return Err(not_a_factor(next, name));
            }
        }
        Some(offset) => {
            let (previous, _, name) = existing[offset];
            if modulus % previous != 0 {
                return Err(modulus_error(format!(
                    "The new modulus {modulus} is not divisible by {previous}, the modulus of existing partition \"{name}\"."
                )));
            }
            if let Some((next, _, name)) = existing.get(offset + 1) {
                if next % modulus != 0 {
                    return Err(not_a_factor(*next, name));
                }
            }
        }
    }
    let greatest = existing
        .iter()
        .map(|(modulus, _, _)| *modulus)
        .max()
        .unwrap_or(modulus);
    // The smallest shared residue is the first coverage position PostgreSQL checks.
    // Equal positions retain the existing modulus/remainder order.
    Ok(existing
        .iter()
        .filter_map(|(existing_modulus, existing_remainder, name)| {
            super::comparison::first_shared_hash_residue(
                modulus,
                remainder,
                *existing_modulus,
                *existing_remainder,
                greatest,
            )
            .map(|residue| (residue, *name))
        })
        .min_by_key(|(residue, _)| *residue)
        .map(|(_, name)| name.clone()))
}

fn modulus_error(detail: String) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "42P17".into(),
        message: "every hash partition modulus must be a factor of the next larger modulus".into(),
        detail: Some(detail),
        hint: None,
    }
}

/// Relation names in partition diagnostics are unqualified, as `RelationGetRelationName` reports them.
pub(super) fn local_relation_name(table: &str) -> Result<String, SQLError> {
    uqa_core::RelationIdentity::from_legacy_name(table)
        .map(|identity| identity.name)
        .map_err(SQLError::Internal)
}

fn invalid_object_definition(message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P17".into(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::{hash_overlap, Sibling};
    use crate::ast::PartitionBound;

    fn hash(name: &str, modulus: i32, remainder: i32) -> Sibling {
        Sibling {
            name: name.into(),
            bound: PartitionBound::Hash { modulus, remainder },
        }
    }

    #[test]
    fn hash_admission_follows_check_new_partition_bound() {
        let existing = [hash("mod4_r0", 4, 0), hash("mod8_r2", 8, 2)];
        let not_a_factor = hash_overlap(3, 1, &existing).unwrap_err();
        assert_eq!(
            not_a_factor.detail(),
            Some("The new modulus 3 is not a factor of 4, the modulus of existing partition \"mod4_r0\".")
        );
        let not_divisible = hash_overlap(12, 1, &existing).unwrap_err();
        assert_eq!(
            not_divisible.detail(),
            Some("The new modulus 12 is not divisible by 8, the modulus of existing partition \"mod8_r2\".")
        );
        // Modulus 2, remainder 0 covers remainders 0, 2, 4 and 6 of the greatest modulus 8; the first, 0, belongs to mod4_r0.
        assert_eq!(
            hash_overlap(2, 0, &existing).unwrap(),
            Some("mod4_r0".into())
        );
        assert_eq!(hash_overlap(8, 6, &existing).unwrap(), None);
        assert_eq!(
            hash_overlap(16, 10, &existing).unwrap(),
            Some("mod8_r2".into())
        );
    }

    #[test]
    fn hash_admission_preserves_coverage_order_without_remainder_enumeration() {
        let existing = [hash("later", 8, 6), hash("earlier", 16, 2)];
        assert_eq!(
            hash_overlap(2, 0, &existing).unwrap(),
            Some("earlier".into())
        );
        assert_eq!(
            hash_overlap(i32::MAX, 1, &[hash("zero", i32::MAX, 0)]).unwrap(),
            None
        );
        assert_eq!(
            hash_overlap(2, 1, &[hash("zero", 1 << 30, 0)]).unwrap(),
            None
        );
        for greatest in [2, 4, 8, 16, 32] {
            for modulus in 1..=greatest {
                if greatest % modulus != 0 {
                    continue;
                }
                for remainder in 0..modulus {
                    for existing_remainder in 0..greatest {
                        let expected = (0..greatest).any(|value| {
                            value % modulus == remainder && value == existing_remainder
                        });
                        let actual = hash_overlap(
                            modulus,
                            remainder,
                            &[hash("existing", greatest, existing_remainder)],
                        )
                        .unwrap();
                        assert_eq!(actual.is_some(), expected);
                    }
                }
            }
        }
    }
}
