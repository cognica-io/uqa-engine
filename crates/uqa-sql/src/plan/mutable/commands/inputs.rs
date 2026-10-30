//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Optional mutation inputs transfer their payload owners while preserving existing enclosing boxes.

use super::{CommandPlan, MutableCommandPlan, RewriteMemory, SQLError};
use crate::plan::{
    copy::Copying, MutableSourcePlan, OwnedScalarExpr, ScalarQueryArena, SourcePlan,
};
use uqa_core::memory::ProductionControl;

impl MutableCommandPlan {
    /// Edit an UPDATE/DELETE input, including installation where no FROM/USING clause existed. A retained source reuses its original command-owned box.
    pub fn map_source_input(
        mut self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(Option<MutableSourcePlan>) -> Result<Option<MutableSourcePlan>, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let input = source(&mut self.command)?.as_mut().map(|source| {
            let value = std::mem::replace(source.as_mut(), super::super::sources::empty_source());
            let memory = self
                .memory
                .as_mut()
                .and_then(|memory| super::children::source_memory(memory).take());
            MutableSourcePlan {
                source: value,
                memory,
            }
        });
        let output = map(input)?;
        control.check()?;
        if let Some(output) = output {
            output.assert_owner(control);
            control.check()?;
            if let Some(source) = source(&mut self.command)?.as_mut() {
                **source = output.source;
                if let Some(memory) = &mut self.memory {
                    *super::children::source_memory(memory) = output.memory;
                }
            } else {
                let (value, memory) = if let Some(mut copying) = Copying::for_rewrite(*control) {
                    let value = copying.boxed_source(output)?;
                    (value, Some(copying.into_rewrite_memory()))
                } else {
                    let (value, _) = output.into_rewrite_parts(control)?;
                    (Box::new(value), None)
                };
                *source(&mut self.command)? = Some(value);
                if let Some(owner) = &mut self.memory {
                    *body(owner).field_mut("source") = memory;
                }
            }
        } else {
            *source(&mut self.command)? = None;
            if let Some(memory) = &mut self.memory {
                *body(memory).field_mut("source") = None;
            }
        }
        Ok(self)
    }

    /// Edit a possibly absent qualification together with its original scalar-query namespace. The callback owns the detached scalar; new query occurrences are admitted by the arena before attachment.
    pub fn map_predicate_input(
        mut self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(
            Option<OwnedScalarExpr>,
            &mut ScalarQueryArena,
        ) -> Result<Option<OwnedScalarExpr>, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let (predicate, queries) = predicate_and_queries(&mut self.command)?;
        let predicate = predicate.take().map(|expression| {
            let memory = self
                .memory
                .as_mut()
                .and_then(|memory| {
                    body(memory)
                        .field_mut("predicate")
                        .as_mut()
                        .expect("predicate constructor")
                        .scalar_mut()
                        .take()
                })
                .map(|memory| match memory {
                    RewriteMemory::Scalar(memory) => memory,
                    RewriteMemory::Fields { .. } => unreachable!("predicate scalar owner"),
                });
            OwnedScalarExpr::from_rewrite_parts(expression, memory)
        });
        let mut arena = ScalarQueryArena {
            queries: std::mem::take(queries),
            memory: self
                .memory
                .as_mut()
                .and_then(|memory| body(memory).field_mut("subqueries").take()),
        };
        let output = map(predicate, &mut arena)?;
        control.check()?;
        let (output, memory) = if let Some(mut copying) = Copying::for_rewrite(*control) {
            let output = output.map(|value| copying.take_scalar(value)).transpose()?;
            (output, Some(copying.into_rewrite_memory()))
        } else {
            let output = output
                .map(|value| {
                    value
                        .finish(control)
                        .map(|value| value.into_uncontrolled().expect("ordinary predicate"))
                })
                .transpose()?;
            (output, None)
        };
        let (predicate, queries) = predicate_and_queries(&mut self.command)?;
        *predicate = output;
        *queries = arena.queries;
        if let Some(owner) = &mut self.memory {
            let body = body(owner);
            *body.field_mut("predicate") = memory;
            *body.field_mut("subqueries") = arena.memory;
        }
        Ok(self)
    }
}

fn body(memory: &mut RewriteMemory) -> &mut RewriteMemory {
    memory.field_mut("value0").as_mut().expect("mutation body")
}

fn source(command: &mut CommandPlan) -> Result<&mut Option<Box<SourcePlan>>, SQLError> {
    match command {
        CommandPlan::Update(plan) => Ok(&mut plan.source),
        CommandPlan::Delete(plan) => Ok(&mut plan.source),
        _ => Err(SQLError::Internal(
            "optional source edit requires UPDATE or DELETE".into(),
        )),
    }
}

fn predicate_and_queries(
    command: &mut CommandPlan,
) -> Result<
    (
        &mut Option<crate::ScalarExpr>,
        &mut Vec<crate::plan::QueryPlan>,
    ),
    SQLError,
> {
    match command {
        CommandPlan::Update(plan) => Ok((&mut plan.predicate, &mut plan.subqueries)),
        CommandPlan::Delete(plan) => Ok((&mut plan.predicate, &mut plan.subqueries)),
        _ => Err(SQLError::Internal(
            "qualification edit requires UPDATE or DELETE".into(),
        )),
    }
}
