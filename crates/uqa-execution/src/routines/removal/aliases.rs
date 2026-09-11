//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expand removed columns before preparing surviving routine source-alias candidates.

use super::RoutineRemovalContext;
use std::collections::BTreeSet;
use uqa_sql::{
    ast::{CreateFunction, FunctionBinding},
    routines::lifecycle::rewrites as analysis,
    SQLError,
};

pub fn prepare_routine_column_alias_drop(
    context: &RoutineRemovalContext<'_>,
    mut columns: BTreeSet<(String, String)>,
    removed_routines: &[FunctionBinding],
) -> Result<Vec<CreateFunction>, SQLError> {
    if columns.is_empty() {
        return Ok(Vec::new());
    }
    let mut relations = BTreeSet::new();
    loop {
        let previous = columns.len();
        super::expand_column_drop_dependencies(context, &mut columns, &mut relations)?;
        columns.extend(super::sequence_drop_column_names(context, &relations)?);
        if columns.len() == previous {
            break;
        }
    }
    let dependencies = analysis::routine_column_drop_dependencies(columns)?;
    let registry = context.bodies.registry.routine_snapshot();
    analysis::routine_column_alias_drop_candidates(
        context.bodies.columns,
        &registry,
        &dependencies,
        removed_routines,
    )
}
