//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic removal of resolved routines from the routine registry.

use super::{RoutineDropTarget, RoutineRegistry};
use crate::SQLError;

/// Remove the routines `targets` names from `next`. Every target is found before any is removed, so a routine registered after the targets were resolved is retained, a routine re-created with a resolved signature under another identity is not removed, and a removal whose target has disappeared removes nothing.
pub fn remove_routine_registry_targets(
    next: &mut RoutineRegistry,
    targets: &[RoutineDropTarget],
) -> Result<(), SQLError> {
    for target in targets {
        if !next
            .get(&target.name)
            .is_some_and(|overloads| overloads.iter().any(|function| target.names(function)))
        {
            return Err(SQLError::Internal(format!(
                "resolved {} {} disappeared before DROP",
                target.kind(),
                target.label()
            )));
        }
    }
    for target in targets {
        if let Some(overloads) = next.get_mut(&target.name) {
            overloads.retain(|function| !target.names(function));
            if overloads.is_empty() {
                next.remove(&target.name);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
