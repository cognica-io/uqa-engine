//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain catalog tuple identity while replacing a routine's changed composite input datums and compiled SQL body.

use super::RoutineMutationContext;
use uqa_sql::{expr::composites::constants::CompositeConstantChange, SQLError};

pub fn rewrite_composite_constants(
    context: &RoutineMutationContext<'_>,
    change: &CompositeConstantChange<'_>,
) -> Result<(), SQLError> {
    let before = context.registry.routine_snapshot();
    let mut next = before.clone();
    let mut changed = false;
    for (name, overloads) in &before {
        let mut replacements = None;
        for (index, routine) in overloads.iter().enumerate() {
            let Some((definition, body)) = routine.rewritten_composite_constants(change)? else {
                continue;
            };
            let replacement = if change.rename.is_some() {
                uqa_sql::routines::SQLUserFunction::new(definition, body).shared()
            } else {
                super::revision::replacement(definition, body)?
            };
            replacements.get_or_insert_with(|| overloads.clone())[index] = replacement;
        }
        if let Some(replacements) = replacements {
            next.insert(name.clone(), replacements)?;
            changed = true;
        }
    }
    if changed {
        context.publication.persist_routine_definitions(&next)?;
        super::publication::record_changes(context.changes, &before, &next);
        **context.registry.routines_write() = next;
        context.changes.catalog_registry_changed();
    }
    Ok(())
}
