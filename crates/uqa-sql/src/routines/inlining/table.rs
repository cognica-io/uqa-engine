//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed SQL set-function bodies eligible to become a caller-owned subquery.

use super::{
    arguments, ensure_routine_execute_privilege, properties, sql_function_result_layout,
    ColumnType, CreateFunction, FunctionBinding, QueryBlockPlan, RelationalPlan,
    RoutineInliningContext, SQLError, SQLFunctionResultKind, ScalarExpr, UnifiedPlan,
};
use crate::ast::FunctionVolatility;
use crate::plan::QueryPlan;

pub struct InlineRoutineQuery {
    pub identity: [u8; 16],
    pub query: Box<QueryPlan>,
    /// Declared OUT names or composite attributes; a scalar result is named by its FROM alias.
    pub declared_columns: Option<Vec<String>>,
}

impl RoutineInliningContext<'_> {
    /// Describe a selected scalar FROM call without evaluating its body or arguments.
    pub fn scalar_from_columns(
        &self,
        binding: &FunctionBinding,
        arguments: &[ScalarExpr],
        output_name: &str,
        alias: Option<&str>,
        columns: &[String],
        ordinality: bool,
    ) -> Result<Option<Vec<String>>, SQLError> {
        let Some(function) = self.selected_function(binding) else {
            return Ok(None);
        };
        if function.def.returns_set()
            || !closed_arguments(self, arguments)?
            || crate::semantics::user_function_composite_result(
                self.routines,
                &function,
                binding.invocation.as_deref(),
            )?
            .is_some()
        {
            return Ok(None);
        }
        let names = crate::semantics::user_table_function_columns(
            self.routines,
            &function,
            binding.invocation.as_deref(),
            output_name,
            alias,
            columns,
            ordinality,
        )?;
        Ok((names.len() == 1 + usize::from(ordinality)).then_some(names))
    }

    /// Expand an ordinary SQL set function under the caller's snapshot. Volatile inputs, row-scope-dependent inputs, security boundaries and ordinality retain the function scan.
    pub fn prepare_table(
        &self,
        binding: &FunctionBinding,
        arguments: &[ScalarExpr],
        active: &[[u8; 16]],
    ) -> Result<Option<InlineRoutineQuery>, SQLError> {
        let Some(function) = self.selected_function(binding) else {
            return Ok(None);
        };
        let Some(identity) = binding.object_id else {
            return Ok(None);
        };
        let Some(invocation) = binding.invocation.as_deref() else {
            return Ok(None);
        };
        let specialized =
            super::super::invocation::specialized_definition(&function.def, invocation)?;
        let definition = specialized.as_ref().unwrap_or(&function.def);
        if active.contains(&identity) || !eligible_definition(definition) {
            return Ok(None);
        }
        let Some(arguments) = arguments::prepare(self, definition, invocation, arguments)? else {
            return Ok(None);
        };
        if !closed_arguments(self, &arguments)? {
            return Ok(None);
        }
        match ensure_routine_execute_privilege(self.authority, definition) {
            Ok(()) => {}
            Err(error) if error.sqlstate() == Some("42501") => return Ok(None),
            Err(error) => return Err(error),
        }
        let Some((plan, result, _)) = self.analyze_plan(&function, definition)? else {
            return Ok(None);
        };
        let UnifiedPlan::Query(mut query) = plan else {
            return Ok(None);
        };
        if !streamable_body(self, &query) {
            return Ok(None);
        }
        let layout = sql_function_result_layout(self.types, definition, Some(&result))?;
        if layout.kind == SQLFunctionResultKind::Void
            || layout.kind == SQLFunctionResultKind::Value
                && matches!(
                    layout.declared_type,
                    ColumnType::Composite(_) | ColumnType::Record | ColumnType::Domain { .. }
                )
        {
            return Ok(None);
        }
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            return Ok(None);
        };
        if !coerce_result(block, &layout, &result) {
            return Ok(None);
        }
        let declared_columns = crate::semantics::user_function_declared_columns(
            self.routines,
            &function,
            Some(invocation),
        )?;
        let mut invalid = false;
        query.rewrite_scalar_expressions(&mut |node| {
            if let ScalarExpr::Param(index) = node {
                if let Some(value) = index.checked_sub(1).and_then(|index| arguments.get(index)) {
                    *node = value.clone();
                } else {
                    invalid = true;
                }
            }
        });
        if invalid {
            return Err(SQLError::Internal(
                "SQL body parameter is outside its invocation".into(),
            ));
        }
        Ok(Some(InlineRoutineQuery {
            identity,
            query,
            declared_columns,
        }))
    }
}

pub(super) fn eligible_definition(definition: &CreateFunction) -> bool {
    definition.language == "sql"
        && definition.returns_set()
        && definition.volatility != FunctionVolatility::Volatile
        && !definition.strict
        && !definition.security.security_definer
        && definition.config.is_empty()
}

fn closed_arguments(
    context: &RoutineInliningContext<'_>,
    arguments: &[ScalarExpr],
) -> Result<bool, SQLError> {
    // Substitution into a new query level needs no scope adjustment for closed expressions or external parameters. Correlated arguments keep their original function scan until the IR carries lexical outer-reference levels.
    for argument in arguments {
        let mut closed = true;
        argument.visit(&mut |node| {
            closed &= !matches!(
                node,
                ScalarExpr::Column(_)
                    | ScalarExpr::QualifiedColumn { .. }
                    | ScalarExpr::InternalColumn(_)
                    | ScalarExpr::ScalarSubquery(_)
                    | ScalarExpr::Exists { .. }
                    | ScalarExpr::InSubquery { .. }
            );
        });
        if !closed
            || properties::volatility(context, argument, &crate::RowSchema::default())?
                == FunctionVolatility::Volatile
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn coerce_result(
    block: &mut QueryBlockPlan,
    layout: &super::super::result_check::SQLFunctionResultLayout,
    result: &crate::binding::statements::AnalyzedResult,
) -> bool {
    let targets = layout.columns.as_ref().map_or_else(
        || vec![layout.declared_type.clone()],
        |columns| columns.iter().map(|column| column.ty.clone()).collect(),
    );
    if targets.len() != block.projections.len() {
        return false;
    }
    let source_types = result.column_types().unwrap_or_default();
    for (index, (projection, target)) in block.projections.iter_mut().zip(targets).enumerate() {
        if source_types.get(index).and_then(Option::as_ref) != Some(&target) {
            projection.expr = ScalarExpr::Cast {
                implicit: true,
                expr: Box::new(projection.expr.clone()),
                ty: target.catalog_name(),
            };
        }
    }
    true
}

fn streamable_body(context: &RoutineInliningContext<'_>, query: &QueryPlan) -> bool {
    if !query.ctes.is_empty() {
        return false;
    }
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return false;
    };
    matches!(block.compute, super::ComputePlan::Project)
        && block.subqueries.is_empty()
        && !block.distinct
        && block.distinct_on.is_empty()
        && block.projections.iter().all(|projection| {
            !crate::semantics::sets::static_setness::expression_may_return_set_statically(
                context.routines,
                &projection.expr,
            )
        })
        && block.from.as_ref().is_none_or(|source| match source {
            crate::plan::SourcePlan::Values { rows, .. } => {
                rows.iter().flatten().all(super::is_constant)
            }
            _ => false,
        })
}
