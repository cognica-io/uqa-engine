//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{ast::JoinKind, plan::SourcePlan, SQLError};
use uqa_core::memory::ProductionControl;

/// Visit nullable visible namespaces without copying relation identifiers or constructing an intermediate set.
pub fn visit_outer_join_nullable_qualifiers_with_control<'a>(
    source: &'a SourcePlan,
    control: &ProductionControl<'_>,
    visit: &mut dyn FnMut(&'a str) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    visit_nullable(source, false, control, visit)
}

fn contains_nullable(
    source: &SourcePlan,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    let SourcePlan::Join {
        left, right, kind, ..
    } = source
    else {
        return Ok(false);
    };
    Ok(
        matches!(kind, JoinKind::Left | JoinKind::Right | JoinKind::Full)
            || contains_nullable(left, control)?
            || contains_nullable(right, control)?,
    )
}

fn visit_nullable<'a>(
    source: &'a SourcePlan,
    inherited: bool,
    control: &ProductionControl<'_>,
    visit: &mut dyn FnMut(&'a str) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    control.check()?;
    match source {
        SourcePlan::Join {
            left,
            right,
            kind,
            alias,
            ..
        } => {
            if let Some(alias) = alias {
                if inherited || contains_nullable(source, control)? {
                    visit(alias)?;
                }
            } else {
                visit_nullable(
                    left,
                    inherited || matches!(kind, JoinKind::Right | JoinKind::Full),
                    control,
                    visit,
                )?;
                visit_nullable(
                    right,
                    inherited || matches!(kind, JoinKind::Left | JoinKind::Full),
                    control,
                    visit,
                )?;
            }
        }
        _ if inherited => {
            if let Some(qualifier) = source.visible_qualifier() {
                visit(qualifier)?;
            }
        }
        _ => {}
    }
    Ok(())
}
