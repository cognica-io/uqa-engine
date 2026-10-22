//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered FROM analysis, including lateral scopes and join coercion.

use super::super::{
    operator_join_relation_schemas, overlay_outer_schema, rename_schema, JoinSchemaBinding,
    SourcePlan, SourceScope,
};
use super::{Preparation, QueryPlan, RowSchema, SQLError, ScalarExpr};
use crate::ast::{FunctionBinding, OperatorJoinRelations};
use crate::semantics::sets::placement::SetPlacement;

impl Preparation<'_> {
    pub(super) fn source(
        &mut self,
        source: &SourcePlan,
        subqueries: &[QueryPlan],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        self.source_in_scope(source, subqueries, SourceScope::new(outer))
    }

    pub(super) fn source_in_scope(
        &mut self,
        source: &SourcePlan,
        subqueries: &[QueryPlan],
        scope: SourceScope<'_>,
    ) -> Result<RowSchema, SQLError> {
        let schema = self.source_in_scope_inner(source, subqueries, scope)?;
        let schema = if scope.retain_names {
            SourceScope::describe_output(source, schema)
        } else {
            schema
        };
        self.parameters.retain_source_schema(source, &schema);
        Ok(schema)
    }

    fn source_in_scope_inner(
        &mut self,
        source: &SourcePlan,
        subqueries: &[QueryPlan],
        scope: SourceScope<'_>,
    ) -> Result<RowSchema, SQLError> {
        if matches!(source, SourcePlan::Join { .. }) {
            return self.join_source(source, subqueries, scope);
        }
        let input = scope.input(source);
        let outer = input.as_ref();
        match source {
            crate::plan::SourcePlan::RetainedRows(source) => return Ok(source.schema().clone()),
            SourcePlan::Table { .. } => {}
            SourcePlan::Values {
                rows,
                alias,
                column_aliases,
                internal_relation,
                ..
            } => {
                if internal_relation.is_some() {
                    return self.scope.bind_source(
                        self.routines,
                        source,
                        subqueries,
                        &self.parameters.values(),
                        outer,
                    );
                }
                let output = self.values(rows, subqueries, outer)?;
                return Ok(rename_schema(
                    &output.schema(),
                    column_aliases,
                    alias.as_deref(),
                ));
            }
            SourcePlan::Subquery {
                body,
                alias,
                column_aliases,
                ..
            } => {
                let schema = self.query(body, outer)?;
                return Ok(rename_schema(&schema, column_aliases, alias.as_deref()));
            }
            SourcePlan::Function {
                name,
                binding,
                relations,
                args,
                ..
            } => {
                if let Some(selected) = self.source_function(
                    name,
                    binding.as_ref(),
                    relations.as_ref(),
                    args,
                    subqueries,
                    outer,
                )? {
                    self.parameters
                        .retain_source_binding(std::ptr::NonNull::from(binding), selected);
                }
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                self.source_functions(functions, subqueries, outer)?;
            }
            SourcePlan::Join { .. } => unreachable!("joins retain their preceding namespace"),
        }
        self.scope.bind_source(
            self.routines,
            source,
            subqueries,
            &self.parameters.values(),
            outer,
        )
    }

    /// Analyze a join: its left input, its right input, which sees the left one when it is lateral or a function, and its ON condition, which admits no set-returning call.
    fn join_source(
        &mut self,
        source: &SourcePlan,
        subqueries: &[QueryPlan],
        scope: SourceScope<'_>,
    ) -> Result<RowSchema, SQLError> {
        let SourcePlan::Join {
            left,
            right,
            kind,
            on,
            using,
            natural,
            alias,
            column_aliases,
            lateral,
            ..
        } = source
        else {
            unreachable!("join analysis called for a different source kind")
        };
        let left_source = left;
        let left = self.source_in_scope(left_source, subqueries, scope.with_names())?;
        let preceding = scope.right_namespace(left_source, &left, *kind);
        let right = self.source_in_scope(
            right,
            subqueries,
            SourceScope {
                outer: scope.outer,
                preceding: Some(&preceding),
                inherited_lateral: *lateral,
                retain_names: true,
            },
        )?;
        let condition = scope.condition();
        let outer = condition.as_ref();
        if let Some(on) = on {
            let joined = RowSchema::join(&left, &right, std::iter::empty::<String>());
            let input = overlay_outer_schema(&joined, outer);
            self.in_placement(SetPlacement::JoinCondition, |analysis| {
                analysis.require_boolean(on, &input, subqueries, "JOIN/ON")
            })?;
        }
        self.scope.bind_join_output_schema(JoinSchemaBinding {
            routines: self.routines,
            kind: *kind,
            on: None,
            using: using.as_ref(),
            natural: *natural,
            alias: alias.as_deref(),
            column_aliases,
            left: &left,
            right: &right,
            subqueries,
            params: &self.parameters.values(),
            outer,
        })
    }

    fn source_functions(
        &mut self,
        functions: &[crate::plan::TableFunctionPlan],
        subqueries: &[QueryPlan],
        outer: Option<&RowSchema>,
    ) -> Result<(), SQLError> {
        for function in functions {
            if let Some(selected) = self.source_function(
                &function.name,
                function.binding.as_ref(),
                function.relations.as_ref(),
                &function.args,
                subqueries,
                outer,
            )? {
                self.parameters
                    .retain_source_binding(std::ptr::NonNull::from(&function.binding), selected);
            }
        }
        Ok(())
    }

    /// Analyze a function in FROM. Its own result may be a set, but no input of it may, as `transformRangeFunction` requires.
    fn source_function(
        &mut self,
        name: &str,
        binding: Option<&FunctionBinding>,
        relations: Option<&OperatorJoinRelations>,
        args: &[ScalarExpr],
        subqueries: &[QueryPlan],
        outer: Option<&RowSchema>,
    ) -> Result<Option<FunctionBinding>, SQLError> {
        let calls = self.set_calls();
        let selected = self.in_placement(SetPlacement::FromFunction, |analysis| {
            analysis.source_function_inputs(name, binding, relations, args, subqueries, outer)
        })?;
        self.reject_set_inputs(calls, crate::semantics::sets::placement::top_level_from)?;
        Ok(selected)
    }

    fn source_function_inputs(
        &mut self,
        name: &str,
        binding: Option<&FunctionBinding>,
        relations: Option<&OperatorJoinRelations>,
        args: &[ScalarExpr],
        subqueries: &[QueryPlan],
        outer: Option<&RowSchema>,
    ) -> Result<Option<FunctionBinding>, SQLError> {
        let local = crate::semantics::builtin_function_dispatch_name(name);
        if crate::registry::is_operator_join_table_function(&local) {
            let (left, right) = operator_join_relation_schemas(
                &self.scope.catalog,
                &self.scope.resolution,
                relations,
            )?;
            let constant = RowSchema::default();
            for (position, argument) in args.iter().enumerate() {
                let input = match position {
                    0 => &left,
                    1 => &right,
                    _ => &constant,
                };
                self.expression(argument, input, subqueries)?;
            }
            return Ok(None);
        }
        let input = outer.cloned().unwrap_or_default();
        self.call(name, binding, args, &input, subqueries)
    }
}
