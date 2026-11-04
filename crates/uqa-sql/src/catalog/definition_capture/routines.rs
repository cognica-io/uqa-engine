//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine images preserve source bodies and creation-bound SQL without session compilation.

use crate::{
    ast::CreateFunction,
    plan::{copy::SyntaxProduction, UnifiedPlan},
    routines::{CompiledFunctionBody, RoutineBody, SQLUserFunction},
    SQLError,
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::memory::{Produced, ProductionControl};

#[derive(Debug)]
pub struct CapturedRoutine {
    pub definition: CreateFunction,
    /// `None` retains the source-body contract; `Some`, including an empty list, retains the bound SQL body.
    pub bound_body: Option<Vec<UnifiedPlan>>,
}

pub type CapturedRoutines = Vec<(String, Vec<CapturedRoutine>)>;

impl SQLUserFunction {
    pub fn capture_definition(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedRoutine>, SQLError> {
        let output = SyntaxProduction::new(*control);
        let definition = output.capture_stored(&self.def)?;
        let bound_body = match &self.body {
            RoutineBody::Source => None,
            RoutineBody::Bound(body) => match body.as_ref() {
                CompiledFunctionBody::SQL(plans) => Some(output.capture_stored(plans)?),
                // SQL-standard bodies cannot use PL/pgSQL; its compiled form belongs to a session.
                CompiledFunctionBody::PLpgSQL(_) => {
                    return Err(SQLError::Internal(
                        "stored routine contains a session-compiled PL/pgSQL body".into(),
                    ));
                }
            },
        };
        output.finish(CapturedRoutine {
            definition,
            bound_body,
        })
    }
}

pub fn routines(
    values: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    control: &ProductionControl<'_>,
) -> Result<Produced<CapturedRoutines>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let mut names = output.vector(values.len())?;
    for (name, overloads) in values {
        let mut entries = output.vector(overloads.len())?;
        for routine in overloads {
            entries.push(output.take(routine.capture_definition(control)?))?;
        }
        names.push((output.text(name)?, entries.finish()?))?;
    }
    let names = names.finish()?;
    output.finish(names)
}

#[cfg(test)]
mod tests;
