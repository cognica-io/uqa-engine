//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inference binding retains one complete private command and borrows its selected table generation.

use super::{InferenceContext, InsertPlan};
use crate::{
    ast::ColumnDef,
    plan::{
        CommandPlan, CommandScalarSlot, MutableExpressionPlan, OwnedScalarExpr, ScalarQueryArena,
    },
    ColumnType, RowSchema, SQLError, SQLParam, ScalarExpr,
};
use uqa_core::memory::{Produced, ProductionControl};

/// `None` leaves the original INSERT borrowed; a rewritten command retains every original field through its last consumer.
pub fn prepare_inference_predicate_with_control(
    context: InferenceContext<'_>,
    statement: &InsertPlan,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<CommandPlan>>, SQLError> {
    control.check()?;
    let Some(conflict) = statement
        .on_conflict
        .as_ref()
        .filter(|conflict| conflict.predicate.is_some() || !conflict.expressions.is_empty())
    else {
        return Ok(None);
    };
    let mut output = None;
    let found = context.catalog.with_column_metadata_with_control(
        &statement.table,
        control,
        &mut |columns| {
            let schema = RowSchema::with_column_metadata_with_control(
                Some(&statement.target_qualifier),
                columns
                    .iter()
                    .map(|column| (column.name.as_str(), Some(&column.ty), column.collation_oid)),
                control,
            )?;
            let mut command = statement.copy_for_rewrite(control)?;
            for index in 0..conflict.expressions.len() {
                command = command.map_scalar(
                    CommandScalarSlot::ConflictExpression(index),
                    control,
                    |expression| {
                        prepare_expression(
                            context, expression, statement, &schema, columns, params, control,
                        )
                    },
                )?;
            }
            command = command.map_scalar(
                CommandScalarSlot::ConflictInferencePredicate,
                control,
                |expression| {
                    prepare_expression(
                        context, expression, statement, &schema, columns, params, control,
                    )
                },
            )?;
            output = Some(command.finish_inference_columns(control)?);
            Ok(())
        },
    )?;
    if !found {
        return Err(SQLError::UnknownTable(
            control.copy_text(&statement.table)?.into_parts().0,
        ));
    }
    Ok(Some(output.expect("selected inference table")))
}

fn prepare_expression(
    context: InferenceContext<'_>,
    expression: OwnedScalarExpr,
    statement: &InsertPlan,
    schema: &RowSchema,
    columns: &[ColumnDef],
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let mut has_subquery = false;
    let mut has_window = false;
    expression.try_visit(&mut |node| {
        control.check()?;
        has_subquery |= matches!(
            node,
            ScalarExpr::ScalarSubquery(_)
                | ScalarExpr::Exists { .. }
                | ScalarExpr::InSubquery { .. }
        );
        has_window |= matches!(node, ScalarExpr::WindowCall { .. });
        Ok::<_, SQLError>(true)
    })?;
    if has_subquery {
        return Err(predicate_error(
            "0A000",
            "cannot use subquery in index inference",
            control,
        )?);
    }
    if crate::semantics::aggregates::contains_aggregate_with_control(
        context.aggregates,
        &expression,
        control,
    )? {
        return Err(predicate_error(
            "42803",
            "aggregate functions are not allowed in index inference",
            control,
        )?);
    }
    if has_window {
        return Err(predicate_error(
            "42P20",
            "window functions are not allowed in index inference",
            control,
        )?);
    }
    let plan = MutableExpressionPlan::from_parts(
        expression,
        ScalarQueryArena::copy_from(&[], control)?,
        control,
    )?;
    let binding = context.binding.binding_scope_with_control(control)?;
    let plan = crate::binding::bind_expression_plan_routines_for_storage_owned(
        context.routines,
        plan,
        params,
        binding.context_with_control(control)?,
        schema,
        control,
    )?;
    let (expression, _) = plan.into_parts(control)?;
    let expression = expression.transform_nodes(control, &mut |_, expression| {
        if matches!(&*expression, ScalarExpr::QualifiedColumn { qualifier, .. } if qualifier == &statement.target_qualifier) {
            return expression.remove_reference_qualifier(control).map_err(Into::into);
        }
        Ok(expression)
    })?;
    let expression = expression.transform_nodes(control, &mut |_, expression| {
        if let ScalarExpr::Cast { expr, ty, .. } = &*expression {
            if let ScalarExpr::Column(name) = expr.as_ref() {
                for column in columns {
                    control.check()?;
                    if column.name == *name {
                        match ColumnType::from_sql_name_with_control(ty, control) {
                            Ok(parsed) if *parsed == column.ty => {
                                return Ok(expression.select_child(0))
                            }
                            Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                                return Err(error)
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        Ok(expression)
    })?;
    inference_identity_owned(expression, control)
}

pub(super) fn inference_identity_owned(
    expression: OwnedScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    expression.transform_nodes(control, &mut |_, mut expression| {
        while matches!(&*expression, ScalarExpr::Collated { .. }) {
            control.check()?;
            expression = expression.select_child(0);
        }
        if matches!(&*expression, ScalarExpr::Cast { .. }) {
            expression = expression.set_cast_implicit(false, control)?;
        }
        if matches!(
            &*expression,
            ScalarExpr::Func {
                binding: Some(_),
                ..
            }
        ) {
            expression = expression.set_call_input_collation(None, control)?;
        }
        Ok(expression)
    })
}

fn predicate_error(
    sqlstate: &str,
    message: &str,
    control: &ProductionControl<'_>,
) -> Result<SQLError, SQLError> {
    let sqlstate = control.copy_text(sqlstate)?;
    let message = control.copy_text(message)?;
    Ok(SQLError::Routine {
        sqlstate: sqlstate.into_parts().0,
        message: message.into_parts().0,
    })
}
