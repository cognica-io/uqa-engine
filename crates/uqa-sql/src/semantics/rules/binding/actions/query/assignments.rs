//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lazy typed NEW expressions for INSERT and UPDATE assignments.

use super::{InternalRelationId, SQLError, ScalarExpr};
use crate::{
    ast::ColumnDef,
    plan::{ExpressionPlan, InsertPlan},
    semantics::rules::analysis::RuleAnalysisContext,
    ColumnType,
};
use uqa_core::memory::ProductionControl;

mod owned;
pub(super) use owned::{column_expression_with_control, update_column_expression_with_control};

pub(super) fn column_expression(
    context: RuleAnalysisContext<'_>,
    insert: &InsertPlan,
    column: &ColumnDef,
    relation: InternalRelationId,
) -> Result<ExpressionPlan, SQLError> {
    let control = ProductionControl::uncontrolled();
    Ok(
        column_expression_with_control(context, insert, column, relation, &control)?
            .finish(&control)?
            .into_uncontrolled()
            .expect("ordinary rule assignment"),
    )
}

pub(super) fn update_column_expression(
    context: RuleAnalysisContext<'_>,
    update: &crate::plan::UpdatePlan,
    column: &ColumnDef,
    old: ScalarExpr,
) -> Result<ExpressionPlan, SQLError> {
    let control = ProductionControl::uncontrolled();
    Ok(update_column_expression_with_control(
        context,
        update,
        column,
        old.copy_for_rewrite(&control)?,
        &control,
    )?
    .finish(&control)?
    .into_uncontrolled()
    .expect("ordinary rule assignment"))
}

fn base_type(mut ty: &ColumnType) -> &ColumnType {
    while let ColumnType::Domain { base, .. } = ty {
        ty = base;
    }
    ty
}
