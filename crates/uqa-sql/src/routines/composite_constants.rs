//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Replace changed composite datums while retaining creation-bound routine syntax.

use super::{CompiledFunctionBody, RoutineBody, SQLUserFunction};
use crate::{
    ast::{CreateFunction, FunctionBody},
    expr::composites::constants::CompositeConstantChange,
    SQLError,
};

impl SQLUserFunction {
    pub fn rewritten_composite_constants(
        &self,
        change: &CompositeConstantChange<'_>,
    ) -> Result<Option<(CreateFunction, RoutineBody)>, SQLError> {
        let mut definition = self.def.clone();
        let mut revised = false;
        for parameter in &mut definition.params {
            if let Some(default) = &mut parameter.default {
                revised |= change.expression(default)?;
            }
        }
        if let FunctionBody::Statements(statements) = &mut definition.body {
            for statement in statements {
                revised |= change.statement_in_scope(statement, Some(&self.def), None)?;
            }
        }
        if !revised {
            return Ok(None);
        }
        let body = match &self.body {
            RoutineBody::Source => RoutineBody::Source,
            RoutineBody::Bound(compiled) => {
                let mut compiled = (***compiled).clone();
                if let CompiledFunctionBody::SQL(plans) = &mut compiled {
                    for plan in plans {
                        if let Some(rename) = change.rename {
                            rename.bind_routine_plan(plan, &self.def)?;
                        }
                        change.plan(plan)?;
                    }
                }
                RoutineBody::Bound(compiled.shared())
            }
        };
        Ok(Some((definition, body)))
    }
}
