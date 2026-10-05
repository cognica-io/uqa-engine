//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unpublish removed routines while retaining the latest registry write guard through persistence.

use super::RoutineRemovalContext;
use uqa_sql::routines::lifecycle::{dependencies as analysis_dependencies, RoutineDropTarget};
use uqa_sql::SQLError;

pub fn commit_routine_registry_drop(
    context: &RoutineRemovalContext<'_>,
    targets: &[RoutineDropTarget],
) -> Result<(), SQLError> {
    if targets.is_empty() {
        return Ok(());
    }
    let mut registry = context.registry.routines_write();
    let mut next = registry.clone();

    analysis_dependencies::remove_routine_registry_targets(&mut next, targets)?;
    context.publication.persist_routine_definitions(&next)?;
    super::super::catalog::publication::record_changes(context.changes, &registry, &next);
    **registry = next;
    drop(registry);
    context.changes.catalog_registry_changed();
    Ok(())
}
