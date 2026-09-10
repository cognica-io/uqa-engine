//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Set-returning expression binding, validation, and dependency rewriting.

use crate::ast::FunctionBinding;
use crate::plan::{AggregateClassifier, ProjectionTarget, QueryBlockPlan};
use crate::routines::RoutineResolution;
use crate::{RowSchema, SQLError, SQLParam, ScalarExpr};

/// Routine metadata and aggregate classification required by set-returning expression analysis.
pub trait SetFunctionCatalog: RoutineResolution + AggregateClassifier {}
impl<T: RoutineResolution + AggregateClassifier + ?Sized> SetFunctionCatalog for T {}

pub type PhysicalProjection = (ProjectionTarget, ScalarExpr);

pub mod rewrite;
pub mod validation;
use rewrite::rewrite_set_calls;

#[derive(Clone)]
pub struct SetFunctionCall {
    pub placeholder: crate::ast::InternalColumnRef,
    pub name: String,
    pub binding: Option<FunctionBinding>,
    pub args: Vec<ScalarExpr>,
    pub level: usize,
}

pub struct SetProjectionPlan {
    pub projections: Vec<PhysicalProjection>,
    pub calls: Vec<SetFunctionCall>,
}

pub struct AggregateOutputProjectionPlan {
    pub statement: QueryBlockPlan,
    pub projections: Vec<PhysicalProjection>,
}

pub struct GroupSetProjectionPlan {
    pub statement: QueryBlockPlan,
    pub projections: Vec<PhysicalProjection>,
}

impl SetProjectionPlan {
    pub fn new(
        engine: &dyn SetFunctionCatalog,
        resolver: &dyn crate::FunctionTypeResolver,
        projections: Vec<PhysicalProjection>,
        schema: &RowSchema,
        params: &[SQLParam],
    ) -> Result<Self, SQLError> {
        let mut calls = Vec::new();
        let call_relation = crate::ast::InternalRelationId::allocate();
        let projections = projections
            .into_iter()
            .map(|(target, expression)| {
                Ok((
                    target,
                    rewrite_set_calls(
                        engine,
                        resolver,
                        expression,
                        &mut calls,
                        call_relation,
                        schema,
                        params,
                    )?,
                ))
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        debug_assert!(!calls.is_empty());
        Ok(Self { projections, calls })
    }
}

pub mod static_setness;
