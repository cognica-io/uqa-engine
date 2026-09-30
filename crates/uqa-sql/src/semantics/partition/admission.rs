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
use crate::SQLError;
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
        PartitionBound::List(values) => list_overlap(values, &siblings)?,
        PartitionBound::Range { lower, upper } => {
            if compare_range_points(lower, upper)? != Ordering::Less {
                let types = keys.iter().map(|key| key.ty.clone()).collect::<Vec<_>>();
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
            range_overlap(lower, upper, &siblings)?
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
    values: &[crate::ast::Expr],
    siblings: &[Sibling],
) -> Result<Option<String>, SQLError> {
    for value in values {
        let value = stored_datum(value)?;
        for sibling in siblings {
            let PartitionBound::List(existing) = &sibling.bound else {
                continue;
            };
            for candidate in existing {
                if stored_datum(candidate)?.cmp(value) == Ordering::Equal {
                    return Ok(Some(sibling.name.clone()));
                }
            }
        }
    }
    Ok(None)
}

/// The partition containing the new lower bound, or else the next partition when the new range does not fit in the gap before it: the first overlapping partition in bound order.
fn range_overlap(
    lower: &[PartitionRangeDatum],
    upper: &[PartitionRangeDatum],
    siblings: &[Sibling],
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
    let mut ordering_error = None;
    ranges.sort_by(|left, right| {
        compare_range_points(left.0, right.0).unwrap_or_else(|error| {
            ordering_error.get_or_insert(error);
            Ordering::Equal
        })
    });
    if let Some(error) = ordering_error {
        return Err(error);
    }
    for (existing_lower, existing_upper, name) in ranges {
        if compare_range_points(lower, existing_upper)? == Ordering::Less
            && compare_range_points(existing_lower, upper)? == Ordering::Less
        {
            return Ok(Some(name.clone()));
        }
    }
    Ok(None)
}

/// Compare two range bound points datum by datum, where `MINVALUE` precedes and `MAXVALUE` follows every value.
pub(super) fn compare_range_points(
    left: &[PartitionRangeDatum],
    right: &[PartitionRangeDatum],
) -> Result<Ordering, SQLError> {
    if left.len() != right.len() {
        return Err(SQLError::Internal(
            "partition range points have different widths".into(),
        ));
    }
    for (left, right) in left.iter().zip(right) {
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
                stored_datum(left)?.cmp(stored_datum(right)?)
            }
        };
        if ordering != Ordering::Equal {
            return Ok(ordering);
        }
    }
    Ok(Ordering::Equal)
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
    let mut covered = if remainder >= greatest {
        remainder % greatest
    } else {
        remainder
    };
    loop {
        if let Some((_, _, name)) = existing
            .iter()
            .find(|(modulus, remainder, _)| covered % modulus == *remainder)
        {
            return Ok(Some((*name).clone()));
        }
        covered += modulus;
        if covered >= greatest {
            return Ok(None);
        }
    }
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
}
