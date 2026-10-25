//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lazy typed NEW expressions for partial INSERT assignments.

use super::{InternalRelationId, SQLError, ScalarExpr};
use crate::{
    assignment::targets::{self, AssignmentLevel},
    ast::{
        AssignmentStep, AssignmentTarget, ColumnDef, FunctionBinding, FunctionDispatch,
        FunctionOrderSyntax,
    },
    plan::{ExpressionPlan, InsertPlan},
    semantics::rules::analysis::RuleAnalysisContext,
    ColumnType,
};
use uqa_core::Value;

pub(super) fn column_expression(
    context: RuleAnalysisContext<'_>,
    insert: &InsertPlan,
    column: &ColumnDef,
    relation: InternalRelationId,
) -> Result<ExpressionPlan, SQLError> {
    let base = base_type(&column.ty);
    let mut current = ScalarExpr::TypedLiteral {
        value: Value::Null,
        ty: base.catalog_name(),
        bound_type: Some(base.clone()),
        parameter_index: None,
        collation_oid: column.collation_oid,
        composite_source: None,
    };
    for (position, target) in insert
        .columns
        .iter()
        .enumerate()
        .filter(|(_, target)| target.column == column.name)
    {
        let value = ScalarExpr::InternalColumn(relation.column(position));
        current = assignment(context, target, &column.ty, current, value)?;
    }
    let mut expression = ExpressionPlan {
        scalar: cast(current, &column.ty),
        subqueries: insert.subqueries.clone(),
    };
    crate::plan::subqueries::prune_expression(&mut expression);
    Ok(expression)
}

fn assignment(
    context: RuleAnalysisContext<'_>,
    target: &AssignmentTarget<ScalarExpr>,
    ty: &ColumnType,
    current: ScalarExpr,
    value: ScalarExpr,
) -> Result<ScalarExpr, SQLError> {
    if target.is_whole_column() {
        return Ok(cast(value, ty));
    }
    let types = targets::field_assignment_types(target, ty, context.composites)?;
    if !targets::has_field_step(target) {
        targets::validate_assignment_result(target, ty)?;
    }
    let assignment = crate::assignment::path::bind(target, &types, context.composites)?;
    let mut args = vec![current];
    for level in targets::assignment_levels(&target.indirection) {
        let AssignmentLevel::Subscripts(steps) = &level else {
            continue;
        };
        let slice = level.is_slice();
        for step in *steps {
            match step {
                AssignmentStep::Index(index) => {
                    if slice {
                        args.push(ScalarExpr::Literal(Value::Int(1)));
                    }
                    args.push(cast((**index).clone(), &ColumnType::Integer));
                }
                AssignmentStep::Slice { lower, upper } => {
                    for bound in [lower, upper] {
                        args.push(
                            bound
                                .as_deref()
                                .map_or(ScalarExpr::Literal(Value::Null), |bound| {
                                    cast(bound.clone(), &ColumnType::Integer)
                                }),
                        );
                    }
                }
                AssignmentStep::Field(_) => unreachable!("subscript group"),
            }
        }
    }
    args.push(cast(value, types.last().expect("assignment value type")));
    let mut binding = FunctionBinding::dispatched(FunctionDispatch::AssignmentStore);
    binding.set_assignment(assignment);
    Ok(call(binding, args))
}

fn base_type(mut ty: &ColumnType) -> &ColumnType {
    while let ColumnType::Domain { base, .. } = ty {
        ty = base;
    }
    ty
}

fn cast(expr: ScalarExpr, ty: &ColumnType) -> ScalarExpr {
    ScalarExpr::Cast {
        implicit: true,
        expr: Box::new(expr),
        ty: ty.catalog_name(),
    }
}

fn call(binding: FunctionBinding, args: Vec<ScalarExpr>) -> ScalarExpr {
    ScalarExpr::Func {
        name: binding.name.clone(),
        binding: Some(binding),
        args,
        order_syntax: FunctionOrderSyntax::Ordinary,
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}
