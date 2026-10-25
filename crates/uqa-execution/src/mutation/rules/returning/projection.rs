//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The outer RETURNING expression belongs to the action's row delivery boundary.

use crate::{
    mutation::returning::{
        build_returning_value_row, DmlReturningShape, ReturningDestination,
        ReturningExecutionContext, ReturningValueProjectionRow,
    },
    OwnedPhysicalRow,
};
use uqa_core::Value;
use uqa_sql::SQLError;

pub trait RuleReturningProjection {
    fn project(
        &self,
        values: &[Value],
        width: usize,
        source: Option<&OwnedPhysicalRow>,
    ) -> Result<OwnedPhysicalRow, SQLError>;

    fn destination(&self) -> Option<ReturningDestination<'_>>;
}

pub struct RuleOutputProjection<'a, S: Clone + 'static> {
    pub context: ReturningExecutionContext<'a, S>,
    pub shape: DmlReturningShape<'a, S>,
}

impl<'a, S: Clone + 'static> RuleOutputProjection<'a, S> {
    pub fn insert(
        context: ReturningExecutionContext<'a, S>,
        plan: &'a uqa_sql::plan::InsertPlan,
        params: &'a [uqa_sql::SQLParam],
        ctes: &'a crate::query::CteScope<S>,
    ) -> Self {
        Self {
            context,
            shape: DmlReturningShape {
                table: &plan.table,
                target_qualifier: &plan.target_qualifier,
                aliases: &plan.returning_aliases,
                returning: &plan.returning,
                params,
                ctes,
                supplemental_schema: None,
            },
        }
    }

    pub fn update(
        context: ReturningExecutionContext<'a, S>,
        plan: &'a uqa_sql::plan::UpdatePlan,
        params: &'a [uqa_sql::SQLParam],
        ctes: &'a crate::query::CteScope<S>,
    ) -> Self {
        Self {
            context,
            shape: DmlReturningShape {
                table: &plan.table,
                target_qualifier: &plan.target_qualifier,
                aliases: &plan.returning_aliases,
                returning: &plan.returning,
                params,
                ctes,
                supplemental_schema: None,
            },
        }
    }

    pub fn delete(
        context: ReturningExecutionContext<'a, S>,
        plan: &'a uqa_sql::plan::DeletePlan,
        params: &'a [uqa_sql::SQLParam],
        ctes: &'a crate::query::CteScope<S>,
    ) -> Self {
        Self {
            context,
            shape: DmlReturningShape {
                table: &plan.table,
                target_qualifier: &plan.target_qualifier,
                aliases: &plan.returning_aliases,
                returning: &plan.returning,
                params,
                ctes,
                supplemental_schema: None,
            },
        }
    }
    pub fn with_table(mut self, table: &'a str) -> Self {
        self.shape.table = table;
        self
    }
}

impl<S: Clone + 'static> RuleReturningProjection for RuleOutputProjection<'_, S> {
    fn project(
        &self,
        values: &[Value],
        width: usize,
        source: Option<&OwnedPhysicalRow>,
    ) -> Result<OwnedPhysicalRow, SQLError> {
        build_returning_value_row(
            self.context,
            ReturningValueProjectionRow {
                table: self.shape.table,
                target_qualifier: self.shape.target_qualifier,
                current: &values[..width],
                old: Some(&values[width..width * 2]),
                new: Some(&values[width * 2..width * 3]),
                aliases: self.shape.aliases,
                context: source,
            },
            self.shape.returning,
            self.shape.params,
            self.shape.ctes,
        )
    }

    fn destination(&self) -> Option<ReturningDestination<'_>> {
        self.context.destination
    }
}
