//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rule assignment expressions retain their scalar children and query namespaces during typed container reconstruction.

use crate::{
    assignment::targets::{self, AssignmentLevel},
    ast::{
        AssignmentStep, AssignmentTarget, ColumnDef, FunctionBinding, FunctionDispatch,
        FunctionValueBinding, InternalRelationId,
    },
    plan::{
        copy::SyntaxProduction, ExpressionPlan, InsertPlan, MutableExpressionPlan, OwnedScalarExpr,
        ScalarQueryArena, UpdatePlan,
    },
    production::Buffer,
    semantics::rules::analysis::RuleAnalysisContext,
    ColumnType, SQLError, ScalarExpr,
};
use uqa_core::{memory::ProductionControl, Value};

pub(in crate::semantics::rules::binding::actions::query) fn column_expression_with_control(
    context: RuleAnalysisContext<'_>,
    insert: &InsertPlan,
    column: &ColumnDef,
    relation: InternalRelationId,
    control: &ProductionControl<'_>,
) -> Result<MutableExpressionPlan, SQLError> {
    control.check()?;
    let base = super::base_type(&column.ty);
    let mut current = OwnedScalarExpr::typed_literal(
        control.copy_value(&Value::Null)?,
        base.catalog_name_with_control(control)?,
        Some(base.clone_with_control(control)?),
        column.collation_oid,
        None,
        control,
    )?;
    for (position, target) in insert
        .columns
        .iter()
        .enumerate()
        .filter(|(_, target)| target.column == column.name)
    {
        let value =
            ScalarExpr::InternalColumn(relation.column(position)).copy_for_rewrite(control)?;
        current = assignment(context, target, &column.ty, current, value, control)?;
    }
    MutableExpressionPlan::from_parts(
        cast(current, &column.ty, control)?,
        ScalarQueryArena::copy_from(&insert.subqueries, control)?,
        control,
    )?
    .prune_subqueries(control)
}

pub(in crate::semantics::rules::binding::actions::query) fn update_column_expression_with_control(
    context: RuleAnalysisContext<'_>,
    update: &UpdatePlan,
    column: &ColumnDef,
    old: OwnedScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<MutableExpressionPlan, SQLError> {
    let mut expression = MutableExpressionPlan::from_parts(
        old,
        ScalarQueryArena::copy_from(&update.subqueries, control)?,
        control,
    )?;
    for input in &update.assignments {
        for target in input
            .target
            .targets()
            .iter()
            .filter(|target| target.column == column.name)
        {
            control.check()?;
            if matches!(input.target, crate::ast::AssignmentTargets::Multiple(_)) {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: "NEW variables in ON UPDATE rules cannot reference columns that are part of a multiple assignment in the subject UPDATE command".into(),
                });
            }
            expression = expression.map_scalar(control, |current, arena| {
                let value = if matches!(input.value, ScalarExpr::Default) {
                    let default = super::super::values::default_expression_with_control(
                        context, column, control,
                    )?;
                    let default = ExpressionPlan::lower_borrowed_for_rewrite(
                        default
                            .as_deref()
                            .unwrap_or(&crate::ast::Expr::Literal(Value::Null)),
                        control,
                    )?;
                    arena.append_expression(default, control)?
                } else {
                    input.value.copy_for_rewrite(control)?
                };
                assignment(context, target, &column.ty, current, value, control)
            })?;
        }
    }
    expression.prune_subqueries(control)
}

fn assignment(
    context: RuleAnalysisContext<'_>,
    target: &AssignmentTarget<ScalarExpr>,
    ty: &ColumnType,
    current: OwnedScalarExpr,
    value: OwnedScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    if target.is_whole_column() {
        return cast(value, ty, control);
    }
    let types =
        targets::field_assignment_types_with_control(target, ty, context.composites, control)?;
    if !targets::has_field_step(target) {
        targets::validate_assignment_result_with_control(target, ty, control)?;
    }
    let assignment =
        crate::assignment::path::bind_with_control(target, &types, context.composites, control)?;
    let mut arguments = Buffer::new(control);
    arguments.push(current, control)?;
    for level in targets::assignment_level_iter(&target.indirection) {
        let AssignmentLevel::Subscripts(steps) = &level else {
            continue;
        };
        let slice = level.is_slice();
        for step in *steps {
            control.check()?;
            match step {
                AssignmentStep::Index(index) => {
                    if slice {
                        arguments.push(
                            ScalarExpr::Literal(Value::Int(1)).copy_for_rewrite(control)?,
                            control,
                        )?;
                    }
                    arguments.push(
                        cast(
                            index.copy_for_rewrite(control)?,
                            &ColumnType::Integer,
                            control,
                        )?,
                        control,
                    )?;
                }
                AssignmentStep::Slice { lower, upper } => {
                    for bound in [lower, upper] {
                        let bound = match bound {
                            Some(bound) => cast(
                                bound.copy_for_rewrite(control)?,
                                &ColumnType::Integer,
                                control,
                            )?,
                            None => ScalarExpr::Literal(Value::Null).copy_for_rewrite(control)?,
                        };
                        arguments.push(bound, control)?;
                    }
                }
                AssignmentStep::Field(_) => unreachable!("subscript group"),
            }
        }
    }
    arguments.push(
        cast(value, types.last().expect("assignment value type"), control)?,
        control,
    )?;
    let output = SyntaxProduction::new(*control);
    let mut binding = output.take(FunctionBinding::dispatched_with_control(
        FunctionDispatch::AssignmentStore,
        control,
    )?);
    let assignment = output.take(assignment);
    binding.value_binding = Some(output.boxed(FunctionValueBinding::Assignment(assignment))?);
    let binding = output.finish(binding)?;
    Ok(OwnedScalarExpr::call(
        control.copy_text(&binding.name)?,
        Some(binding),
        arguments.into_iter(),
        control,
    )?)
}

fn cast(
    value: OwnedScalarExpr,
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    Ok(OwnedScalarExpr::cast(
        value,
        ty.catalog_name_with_control(control)?,
        true,
        control,
    )?)
}
