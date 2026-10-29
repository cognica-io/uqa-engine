//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL table expansion retains independent argument, body, result-layout and source owners.

use super::{
    eligible_definition, FunctionBinding, FunctionVolatility, RoutineInliningContext, SQLError,
    ScalarExpr,
};
use crate::{
    plan::{MutableExecutablePlan, MutableQueryPlan},
    routines::{
        diagnostics::{sql_context_with_control, SQLRoutineStage},
        result_check::{SQLFunctionResultColumn, SQLFunctionResultKind},
    },
    ColumnType,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub struct InlineOwnedRoutineQuery {
    pub identity: [u8; 16],
    pub query: MutableQueryPlan,
    pub declared_columns: Option<Produced<Vec<String>>>,
}

impl RoutineInliningContext<'_> {
    fn can_execute_inline(
        &self,
        definition: &crate::ast::CreateFunction,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        match crate::routines::security::ensure_routine_execute_privilege_with_control(
            self.authority,
            definition,
            control,
        ) {
            Ok(()) => Ok(true),
            Err(error) if error.sqlstate() == Some("42501") => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn table_result_columns(
        &self,
        column_names: &[String],
        column_types: &[String],
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<SQLFunctionResultColumn>>, SQLError> {
        let mut target = ProductionVec::new(*control);
        target.reserve(column_names.len().min(column_types.len()))?;
        for (name, ty) in column_names.iter().zip(column_types) {
            let name = control.copy_text(name)?;
            let ty = self
                .types
                .resolve_catalog_column_type_name_with_control(ty, control)?;
            let (name, name_memory) = name.into_parts();
            let (ty, type_memory) = ty.into_parts();
            target.push_produced(control.finish(
                SQLFunctionResultColumn { name, ty },
                control.combine(name_memory, type_memory),
            )?)?;
        }
        Ok(target.finish()?)
    }

    pub fn prepare_table_owned(
        &self,
        binding: &FunctionBinding,
        arguments: &[ScalarExpr],
        active: &[[u8; 16]],
        column_names: &[String],
        column_types: &[String],
        control: &ProductionControl<'_>,
    ) -> Result<Option<InlineOwnedRoutineQuery>, SQLError> {
        let Some(function) =
            crate::routines::selected_bound_function_with_control(self.routines, binding, control)?
        else {
            return Ok(None);
        };
        let Some(identity) = binding.object_id else {
            return Ok(None);
        };
        let Some(invocation) = binding.invocation.as_deref() else {
            return Ok(None);
        };
        let specialized = crate::routines::invocation::specialized_definition_with_control(
            &function.def,
            invocation,
            control,
        )?;
        let definition = specialized.as_deref().unwrap_or(&function.def);
        if active.contains(&identity) || !eligible_definition(definition) {
            return Ok(None);
        }
        let Some(arguments) = super::super::owned_arguments::prepare(
            self, definition, invocation, arguments, control,
        )?
        else {
            return Ok(None);
        };
        let arguments = arguments.into_container(control)?;
        let ScalarExpr::Or(argument_values) = &*arguments else {
            unreachable!("routine arguments");
        };
        if !substitutable_arguments(self, argument_values, control)? {
            return Ok(None);
        }
        if !self.can_execute_inline(definition, control)? {
            return Ok(None);
        }
        let error_context =
            sql_context_with_control(definition, SQLRoutineStage::Inlining, control)?;
        let Some(body) = self
            .analyze_plan_owned(&function, definition, control)
            .map_err(|error| error.with_context(error_context.as_str()))?
        else {
            return Ok(None);
        };
        let MutableExecutablePlan::Query(query) = body.plan else {
            return Ok(None);
        };
        if query.ctes.iter().any(|cte| cte.body.modifies_data()) {
            return Ok(None);
        }
        let query = self
            .bind_inline_relations_owned(query, control)
            .map_err(|error| error.with_context(error_context.as_str()))?;
        let layout = crate::routines::result_check::sql_function_result_layout_with_control(
            self.types,
            definition,
            Some(&body.result),
            control,
        )
        .map_err(|error| error.with_context(error_context.as_str()))?;
        let overrides = if layout.kind == SQLFunctionResultKind::Tuple && !column_types.is_empty() {
            let target = self.table_result_columns(column_names, column_types, control)?;
            crate::routines::result_check::validate_sql_tuple_result_with_control(
                self.types,
                body.result.column_types().unwrap_or_default(),
                &target,
                control,
            )
            .map_err(|error| error.with_context(error_context.as_str()))?;
            Some(target)
        } else {
            None
        };
        let columns = overrides
            .as_deref()
            .map(Vec::as_slice)
            .or_else(|| layout.columns.as_deref());
        let Some((query, width)) =
            coerce_table_result(query, &body.result, &layout, columns, control)?
        else {
            return Ok(None);
        };
        let declared_columns = match crate::semantics::user_function_declared_columns_with_control(
            self.routines,
            &function,
            Some(invocation),
            control,
        )? {
            Some(columns) => Some(columns),
            None => columns
                .map(|columns| copy_result_names(columns, control))
                .transpose()?,
        };
        let query = super::scopes_owned::substitute(query, argument_values, width, control)?;
        Ok(Some(InlineOwnedRoutineQuery {
            identity,
            query,
            declared_columns,
        }))
    }
}

fn substitutable_arguments(
    context: &RoutineInliningContext<'_>,
    arguments: &[ScalarExpr],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let schema = crate::RowSchema::empty_with_control(control)?;
    for argument in arguments {
        let mut subplan = false;
        argument.visit(&mut |node| {
            subplan |= matches!(
                node,
                ScalarExpr::ScalarSubquery(_)
                    | ScalarExpr::Exists { .. }
                    | ScalarExpr::InSubquery { .. }
            );
        });
        if subplan
            || super::super::properties::volatility_with_control(
                context, argument, &schema, control,
            )? == FunctionVolatility::Volatile
        {
            return Ok(false);
        }
    }
    control.check()?;
    Ok(true)
}

fn coerce_table_result(
    query: MutableQueryPlan,
    result: &crate::binding::statements::AnalyzedResult,
    layout: &crate::routines::result_check::SQLFunctionResultLayout,
    columns: Option<&[SQLFunctionResultColumn]>,
    control: &ProductionControl<'_>,
) -> Result<Option<(MutableQueryPlan, usize)>, SQLError> {
    if layout.kind == SQLFunctionResultKind::Void
        || layout.kind == SQLFunctionResultKind::Value
            && matches!(
                crate::type_resolution::base_type(&layout.declared_type),
                ColumnType::Composite(_) | ColumnType::Record
            )
    {
        return Ok(None);
    }
    let mut targets = ProductionVec::new(*control);
    match columns {
        Some(columns) => {
            targets.reserve(columns.len())?;
            for column in columns {
                targets.push_produced(column.ty.clone_with_control(control)?)?;
            }
        }
        None => targets.push_produced(layout.declared_type.clone_with_control(control)?)?,
    }
    let targets = targets.finish()?;
    let sources = result.column_types().unwrap_or_default();
    if sources.len() != targets.len() {
        return Ok(None);
    }
    let query = super::scopes_owned::coerce_result(query, &targets, sources, control)?;
    Ok(Some((query, targets.len())))
}

fn copy_result_names(
    columns: &[SQLFunctionResultColumn],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, uqa_core::ValueRetentionError> {
    let mut names = ProductionVec::new(*control);
    names.reserve(columns.len())?;
    for column in columns {
        names.push_produced(control.copy_text(&column.name)?)?;
    }
    names.finish()
}
