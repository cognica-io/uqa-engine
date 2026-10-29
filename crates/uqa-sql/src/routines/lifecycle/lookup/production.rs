//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native candidate snapshots admit their vector while sharing immutable routine definitions.

use super::{RoutineRegistry, SQLUserFunction};
use crate::SQLError;
use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub fn lookup_sql_routine_candidates_by_keys_with_control<'a>(
    registry: &RoutineRegistry,
    keys: impl IntoIterator<Item = &'a str>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<Arc<SQLUserFunction>>>>, SQLError> {
    control.check()?;
    let mut candidates = ProductionVec::new(*control);
    for key in keys {
        control.check()?;
        let Some(overloads) = registry.get(key) else {
            continue;
        };
        for function in overloads {
            control.check()?;
            candidates.push_produced(
                control.finish(Arc::clone(function), control.empty_reservation())?,
            )?;
        }
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    Ok(Some(candidates.finish()?))
}
