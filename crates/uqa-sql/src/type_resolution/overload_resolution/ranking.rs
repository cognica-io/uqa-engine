//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared candidate ranking borrows signatures and admits only canonical-name workspace.

use super::{
    canonical_column_type_name_with_control, canonical_routine_type_name_with_control,
    canonical_type_category, canonical_type_is_preferred, routine_type_accepts_implicit_cast,
    RankedFunctionMatch,
};
use crate::{type_resolution::common::base_type, ColumnType};
use uqa_core::{memory::ProductionControl, ValueRetentionError};

/// Apply `PostgreSQL`'s candidate-ranking passes to candidates that already accept the call. Returns `false` when conflicting unknown categories make the call ambiguous before any later unknown position may narrow it.
#[must_use]
pub fn rank_function_matches<T: RankedFunctionMatch>(
    candidates: &mut Vec<T>,
    argument_types: &[Option<ColumnType>],
) -> bool {
    super::rank_function_matches_with_control(
        candidates,
        argument_types,
        &ProductionControl::uncontrolled(),
    )
    .expect("ordinary candidate ranking cannot be cancelled or limited")
}

#[expect(
    clippy::too_many_lines,
    reason = "candidate ranking preserves exactness, preferred types and unknown-position ordering"
)]
pub(in crate::type_resolution) fn rank_function_matches_with_control<T: RankedFunctionMatch>(
    candidates: &mut Vec<T>,
    argument_types: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<bool, ValueRetentionError> {
    control.check()?;
    // Fixed candidates are never removed during this pass, so the comparison can borrow each existing signature without copying its strings or a signature container.
    let mut index = 0;
    while index < candidates.len() {
        control.check()?;
        let mut shadowed = false;
        if candidates[index].is_variadic_expansion() {
            for fixed in candidates
                .iter()
                .filter(|candidate| !candidate.is_variadic_expansion())
            {
                control.check()?;
                if fixed.argument_types() == candidates[index].argument_types() {
                    shadowed = true;
                    break;
                }
            }
        }
        if shadowed {
            candidates.remove(index);
        } else {
            index += 1;
        }
    }
    if argument_types.iter().all(Option::is_some) {
        let raw_exact = candidates
            .iter()
            .any(|candidate| candidate.raw_exact_matches() == argument_types.len());
        if raw_exact {
            retain(candidates, control, |candidate| {
                Ok(candidate.raw_exact_matches() == argument_types.len())
            })?;
            return Ok(true);
        }
    }
    let most_exact = candidates
        .iter()
        .map(RankedFunctionMatch::exact_matches)
        .max()
        .unwrap_or(0);
    retain(candidates, control, |candidate| {
        Ok(candidate.exact_matches() == most_exact)
    })?;
    let most_preferred = candidates
        .iter()
        .map(RankedFunctionMatch::preferred_matches)
        .max()
        .unwrap_or(0);
    retain(candidates, control, |candidate| {
        Ok(candidate.preferred_matches() == most_preferred)
    })?;

    for (index, actual) in argument_types.iter().enumerate() {
        control.check()?;
        if actual.is_some() || candidates.len() <= 1 {
            continue;
        }
        let mut first_category = None;
        let mut different_categories = false;
        let mut string_category = false;
        for candidate in candidates.iter() {
            let category = category(&candidate.argument_types()[index], control)?;
            string_category |= category == 'S';
            different_categories |= first_category.is_some_and(|first| first != category);
            first_category.get_or_insert(category);
        }
        let selected = if string_category {
            'S'
        } else if different_categories {
            return Ok(false);
        } else {
            first_category.expect("unknown ranking has multiple candidates")
        };
        retain(candidates, control, |candidate| {
            Ok(category(&candidate.argument_types()[index], control)? == selected)
        })?;
        let mut has_preferred = false;
        for candidate in candidates.iter() {
            if preferred(&candidate.argument_types()[index], control)? {
                has_preferred = true;
                break;
            }
        }
        if has_preferred {
            retain(candidates, control, |candidate| {
                preferred(&candidate.argument_types()[index], control)
            })?;
        }
    }
    if candidates.len() <= 1 {
        return Ok(true);
    }
    let mut known = argument_types.iter().flatten();
    let Some(first) = known.next() else {
        return Ok(true);
    };
    let identity = canonical_column_type_name_with_control(base_type(first), control)?;
    for ty in known {
        if *canonical_column_type_name_with_control(base_type(ty), control)? != *identity {
            return Ok(true);
        }
    }
    retain(candidates, control, |candidate| {
        Ok(argument_types.iter().enumerate().all(|(index, actual)| {
            actual.is_some()
                || routine_type_accepts_implicit_cast(&identity, &candidate.argument_types()[index])
        }))
    })?;
    Ok(true)
}

fn retain<T>(
    candidates: &mut Vec<T>,
    control: &ProductionControl<'_>,
    mut keep: impl FnMut(&T) -> Result<bool, ValueRetentionError>,
) -> Result<(), ValueRetentionError> {
    let mut error = None;
    candidates.retain(|candidate| {
        if error.is_some() {
            return true;
        }
        match control.check().and_then(|()| keep(candidate)) {
            Ok(keep) => keep,
            Err(failure) => {
                error = Some(failure);
                true
            }
        }
    });
    error.map_or(Ok(()), Err)
}

fn category(name: &str, control: &ProductionControl<'_>) -> Result<char, ValueRetentionError> {
    let name = canonical_routine_type_name_with_control(name, control)?;
    Ok(canonical_type_category(&name))
}

fn preferred(name: &str, control: &ProductionControl<'_>) -> Result<bool, ValueRetentionError> {
    let name = canonical_routine_type_name_with_control(name, control)?;
    Ok(canonical_type_is_preferred(&name))
}

#[cfg(test)]
mod tests;
