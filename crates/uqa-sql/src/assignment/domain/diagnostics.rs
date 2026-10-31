//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain diagnostics admit the selected catalog spelling and message destination.

use super::{AssignmentContext, SQLError};
use uqa_core::memory::ProductionControl;

pub(super) fn violation(
    context: &dyn AssignmentContext,
    oid: u32,
    constraint: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<SQLError, SQLError> {
    let name = context.domain_display_name_with_control(oid, control)?;
    let message = if let Some(constraint) = constraint {
        control.format(format_args!(
            "value for domain {} violates check constraint \"{constraint}\"",
            name.as_str(),
        ))?
    } else {
        control.format(format_args!(
            "domain {} does not allow null values",
            name.as_str(),
        ))?
    };
    let (message, _memory) = message.into_parts();
    Ok(super::domain_error(
        if constraint.is_some() {
            "23514"
        } else {
            "23502"
        },
        message,
    ))
}
