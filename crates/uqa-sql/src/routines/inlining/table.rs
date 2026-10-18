//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed SQL set-function bodies eligible to become a caller-owned subquery.

use super::{
    arguments, ensure_routine_execute_privilege, properties, sql_function_result_layout,
    ColumnType, CreateFunction, FunctionBinding, RoutineInliningContext, SQLError,
    SQLFunctionResultKind, ScalarExpr, UnifiedPlan,
};
use crate::ast::FunctionVolatility;
use crate::plan::QueryPlan;

mod scopes;

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

    /// Expand an ordinary SQL set function under the caller's snapshot. Volatile inputs, subplans, security boundaries and ordinality retain the function scan.
    pub fn prepare_table(
        &self,
        binding: &FunctionBinding,
        arguments: &[ScalarExpr],
        active: &[[u8; 16]],
        column_names: &[String],
        column_types: &[String],
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
        if !substitutable_arguments(self, &arguments)? {
            return Ok(None);
        }
        match ensure_routine_execute_privilege(self.authority, definition) {
            Ok(()) => {}
            Err(error) if error.sqlstate() == Some("42501") => return Ok(None),
            Err(error) => return Err(error),
        }
        let Some((plan, result, _)) =
            self.analyze_plan(&function, definition).map_err(|error| {
                error.with_context(super::super::diagnostics::sql_context(
                    definition,
                    super::super::diagnostics::SQLRoutineStage::Inlining,
                ))
            })?
        else {
            return Ok(None);
        };
        let UnifiedPlan::Query(mut query) = plan else {
            return Ok(None);
        };
        if query.ctes.iter().any(|cte| cte.body.modifies_data()) {
            return Ok(None);
        }
        self.bind_inline_relations(&mut query).map_err(|error| {
            error.with_context(super::super::diagnostics::sql_context(
                definition,
                super::super::diagnostics::SQLRoutineStage::Inlining,
            ))
        })?;
        let layout = self
            .table_result_layout(definition, &result, column_names, column_types)
            .map_err(|error| {
                error.with_context(super::super::diagnostics::sql_context(
                    definition,
                    super::super::diagnostics::SQLRoutineStage::Inlining,
                ))
            })?;
        if layout.kind == SQLFunctionResultKind::Void
            || layout.kind == SQLFunctionResultKind::Value
                && matches!(
                    layout.declared_type,
                    ColumnType::Composite(_) | ColumnType::Record | ColumnType::Domain { .. }
                )
        {
            return Ok(None);
        }
        let targets = layout.columns.as_ref().map_or_else(
            || vec![layout.declared_type.clone()],
            |columns| columns.iter().map(|column| column.ty.clone()).collect(),
        );
        let source_types = result.column_types().unwrap_or_default();
        if source_types.len() != targets.len() {
            return Ok(None);
        }
        query = Box::new(scopes::coerce_result(*query, &targets, source_types));
        let declared_columns = crate::semantics::user_function_declared_columns(
            self.routines,
            &function,
            Some(invocation),
        )?
        .or_else(|| {
            layout
                .columns
                .as_ref()
                .map(|columns| columns.iter().map(|column| column.name.clone()).collect())
        });
        query = scopes::substitute(query, &arguments, targets.len())?;
        Ok(Some(InlineRoutineQuery {
            identity,
            query,
            declared_columns,
        }))
    }
    fn bind_inline_relations(&self, query: &mut QueryPlan) -> Result<(), SQLError> {
        let namespace = self.catalog.stored_query_namespace();
        let mode = if query.relations_bound {
            crate::catalog::resolution::RelationLookupMode::Bound
        } else {
            crate::catalog::resolution::RelationLookupMode::Dynamic
        };
        let mut dependencies = std::collections::BTreeSet::default();
        crate::binding::view_dependencies::bind_query_plan_relations(
            query,
            &namespace.transition_relations,
            &mut |reference| {
                let mut reference = reference.to_string();
                crate::binding::stored_relations::bind_catalog_relation_reference(
                    self.relations,
                    &mut reference,
                    mode,
                    false,
                    "function",
                    &mut dependencies,
                )?;
                Ok::<_, SQLError>(reference)
            },
        )
    }

    fn table_result_layout(
        &self,
        definition: &CreateFunction,
        result: &crate::binding::statements::AnalyzedResult,
        names: &[String],
        types: &[String],
    ) -> Result<super::super::result_check::SQLFunctionResultLayout, SQLError> {
        use super::super::result_check::{
            validate_anonymous_record_result, SQLFunctionResultColumn,
        };
        let mut layout = sql_function_result_layout(self.types, definition, Some(result))?;
        if layout.kind == SQLFunctionResultKind::Tuple && !types.is_empty() {
            let target = names
                .iter()
                .zip(types)
                .map(|(name, ty)| {
                    Ok(SQLFunctionResultColumn {
                        name: name.clone(),
                        ty: self.types.resolve_catalog_column_type_name(ty)?,
                    })
                })
                .collect::<Result<Vec<_>, SQLError>>()?;
            validate_anonymous_record_result(
                self.types,
                result.column_types().unwrap_or_default(),
                &target,
                Some(SQLFunctionResultKind::Tuple),
            )?;
            layout.columns = Some(target);
        }
        Ok(layout)
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
    // Scalar FROM replacement does not introduce attribute capture; row-dependent scalar calls retain their function scan.
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

fn substitutable_arguments(
    context: &RoutineInliningContext<'_>,
    arguments: &[ScalarExpr],
) -> Result<bool, SQLError> {
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
            || properties::volatility(context, argument, &crate::RowSchema::default())?
                == FunctionVolatility::Volatile
        {
            return Ok(false);
        }
    }
    Ok(true)
}
