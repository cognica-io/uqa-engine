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
use uqa_core::{
    memory::{ProductionControl, ProductionVec},
    ValueRetentionError,
};

/// Apply `PostgreSQL`'s candidate-ranking passes to candidates that already accept the call. Returns `false` when conflicting `unknown` categories leave the call ambiguous and the known arguments' type selects no single candidate either.
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

    if candidates.len() <= 1 {
        return Ok(true);
    }
    // `func_select_candidate` settles the `unknown` positions together: each position takes the string category when any candidate accepts it, otherwise the one category every candidate agrees on; a disagreement leaves every position unresolved and skips the stripping, but not the heuristic after it.
    let mut slots = ProductionVec::new(*control);
    let mut resolved = true;
    for (index, actual) in argument_types.iter().enumerate() {
        control.check()?;
        if actual.is_some() {
            continue;
        }
        let mut slot: Option<(char, bool)> = None;
        let mut conflict = false;
        for candidate in candidates.iter() {
            let name = &candidate.argument_types()[index];
            let category = category(name, control)?;
            let preferred = preferred(name, control)?;
            match slot {
                None => slot = Some((category, preferred)),
                Some((current, has_preferred)) if current == category => {
                    slot = Some((current, has_preferred || preferred));
                }
                Some(_) if category == 'S' => slot = Some(('S', preferred)),
                Some(_) => conflict = true,
            }
        }
        let slot = slot.expect("unknown ranking has multiple candidates");
        if conflict && slot.0 != 'S' {
            resolved = false;
            break;
        }
        slots.push_copy((index, slot.0, slot.1))?;
    }
    if resolved {
        let accepts = |candidate: &T| -> Result<bool, ValueRetentionError> {
            for &(index, selected, has_preferred) in slots.iter() {
                let name = &candidate.argument_types()[index];
                if category(name, control)? != selected
                    || (has_preferred && !preferred(name, control)?)
                {
                    return Ok(false);
                }
            }
            Ok(true)
        };
        let mut kept = 0;
        for candidate in candidates.iter() {
            if accepts(candidate)? {
                kept += 1;
            }
        }
        // A rule that rejects every candidate is skipped rather than applied.
        if kept > 0 {
            retain(candidates, control, accepts)?;
        }
        if candidates.len() == 1 {
            return Ok(true);
        }
    }
    // The last heuristic: when every known argument has one type, the `unknown` arguments are assumed to have it too, and a candidate every argument reaches by implicit cast is selected when it is the only one.
    let mut known = argument_types.iter().flatten();
    let Some(first) = known.next() else {
        return Ok(resolved);
    };
    let identity = canonical_column_type_name_with_control(base_type(first), control)?;
    for ty in known {
        if *canonical_column_type_name_with_control(base_type(ty), control)? != *identity {
            return Ok(resolved);
        }
    }
    retain(candidates, control, |candidate| {
        Ok(argument_types.iter().enumerate().all(|(index, actual)| {
            actual.is_some()
                || routine_type_accepts_implicit_cast(&identity, &candidate.argument_types()[index])
        }))
    })?;
    Ok(resolved || candidates.len() == 1)
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
