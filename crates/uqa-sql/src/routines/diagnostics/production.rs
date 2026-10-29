//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine context retained during planning is formatted directly into its admitted text owner.

use super::SQLRoutineStage;
use crate::{ast::CreateFunction, SQLError};
use uqa_core::memory::{Produced, ProductionControl};

pub fn sql_context_with_control(
    definition: &CreateFunction,
    stage: SQLRoutineStage,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    control.check()?;
    let names = crate::compiler::parse_regobject_name_with_control(&definition.name, control)?;
    let name = names
        .as_ref()
        .and_then(|names| names.last())
        .map_or(definition.name.as_str(), String::as_str);
    Ok(match stage {
        SQLRoutineStage::Inlining => {
            control.format(format_args!("SQL function \"{name}\" during inlining"))?
        }
        SQLRoutineStage::Startup => {
            control.format(format_args!("SQL function \"{name}\" during startup"))?
        }
        SQLRoutineStage::Statement(position) => {
            control.format(format_args!("SQL function \"{name}\" statement {position}"))?
        }
    })
}
