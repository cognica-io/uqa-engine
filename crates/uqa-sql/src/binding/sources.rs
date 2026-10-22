//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source row-type adapters and join binding inputs.

use crate::ast::{JoinKind, JoinUsing};
use crate::plan::{QueryPlan, SourcePlan, TableFunctionPlan};
use crate::RowSchema;
use crate::{SQLError, SQLParam};

use super::{
    analysis, lateral_join_scope, operator_join_relation_schemas, projection::rename_schema,
    BindingContext, ScalarExpr, SchemaScope,
};
use crate::routines::RoutineResolution;

pub(super) fn alias_table_schema(
    schema: &RowSchema,
    qualifier: &str,
    column_aliases: &[String],
) -> Result<RowSchema, SQLError> {
    if column_aliases.len() > schema.len() {
        return Err(SQLError::Routine {
            sqlstate: "42P10".into(),
            message: format!(
                "table \"{qualifier}\" has {} columns available but {} columns specified",
                schema.len(),
                column_aliases.len()
            ),
        });
    }
    Ok(rename_schema(schema, column_aliases, Some(qualifier)))
}

impl SchemaScope {
    pub(super) fn alias_relation_schema(
        &self,
        schema: &RowSchema,
        name: &str,
        qualifier: &str,
        column_aliases: &[String],
    ) -> Result<RowSchema, SQLError> {
        let schema = alias_table_schema(schema, qualifier, column_aliases)?;
        match self.catalog.relation_row_type(&self.resolution, name)? {
            Some(row_type) => {
                schema.with_relation_row_type(qualifier, crate::ColumnType::Composite(row_type))
            }
            None => Ok(schema),
        }
    }
}

pub(super) struct JoinSchemaBinding<'a> {
    pub(super) routines: &'a dyn RoutineResolution,
    pub(super) kind: JoinKind,
    pub(super) on: Option<&'a ScalarExpr>,
    pub(super) using: Option<&'a JoinUsing>,
    pub(super) natural: bool,
    pub(super) alias: Option<&'a str>,
    pub(super) column_aliases: &'a [String],
    pub(super) left: &'a RowSchema,
    pub(super) right: &'a RowSchema,
    pub(super) subqueries: &'a [QueryPlan],
    pub(super) params: &'a [SQLParam],
    pub(super) outer: Option<&'a RowSchema>,
}

pub(super) fn table_function_member_source(function: &TableFunctionPlan) -> SourcePlan {
    SourcePlan::Function {
        name: function.name.clone(),
        binding: function.binding.clone(),
        output_name: function.output_name.clone(),
        relations: function.relations.clone(),
        args: function.args.clone(),
        alias: None,
        column_aliases: function.column_aliases.clone(),
        ordinality: false,
        column_types: function.column_types.clone(),
    }
}

/// Derive the exact row type of one FROM source without executing it.
pub fn bind_source_plan_schema(
    routines: &dyn RoutineResolution,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &BindingContext,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::from_context(ctes)?.bind_source(
        routines,
        source,
        ctes.scalar_subqueries,
        params,
        outer,
    )
}

/// Add query-block pseudo columns after the complete source scope is known, so `_meta` is exposed only for one unambiguous local-table source and never shadows a real relation alias.
pub fn with_query_table_pseudo_columns(schema: &RowSchema) -> RowSchema {
    analysis::with_unqualified_table_pseudo_columns(schema)
}

/// Derive and validate one FROM source's exact row type without executing it.
pub fn analyze_source_plan_schema(
    routines: &dyn RoutineResolution,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &BindingContext,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::for_analysis(ctes)?.bind_source(
        routines,
        source,
        ctes.scalar_subqueries,
        params,
        outer,
    )
}

/// Bind every table-function source in one execution-owned source plan to its exact routine identity and return the schema derived from those same bindings.
pub fn bind_source_plan_schema_for_execution(
    routines: &dyn RoutineResolution,
    source: &mut SourcePlan,
    params: &[SQLParam],
    ctes: &BindingContext,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::from_context(ctes)?.bind_source_for_execution(
        routines,
        source,
        ctes.scalar_subqueries,
        params,
        outer,
    )
}

impl SchemaScope {
    pub(super) fn bind_source(
        &mut self,
        routines: &dyn RoutineResolution,
        source: &SourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        let bound = matches!(
            source,
            SourcePlan::Table {
                bound_columns: Some(_),
                ..
            }
        );
        let previous = bound.then(|| {
            self.resolution
                .set_lookup_mode(crate::catalog::resolution::RelationLookupMode::Bound)
        });
        let result = self.bind_source_inner(routines, source, subqueries, params, outer);
        if let Some(previous) = previous {
            self.resolution.set_lookup_mode(previous);
        }
        result
    }
}

impl SchemaScope {
    pub(super) fn bind_source_for_execution(
        &mut self,
        routines: &dyn RoutineResolution,
        source: &mut SourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        match source {
            crate::plan::SourcePlan::RetainedRows(_) => {}
            SourcePlan::Join {
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
            } => {
                let left_schema =
                    self.bind_source_for_execution(routines, left, subqueries, params, outer)?;
                let implicit_lateral_function = matches!(
                    right.as_ref(),
                    SourcePlan::Function { .. } | SourcePlan::FunctionGroup { .. }
                );
                let right_scope = (*lateral || implicit_lateral_function)
                    .then(|| lateral_join_scope(left, &left_schema, outer, *kind));
                let right_schema = self.bind_source_for_execution(
                    routines,
                    right,
                    subqueries,
                    params,
                    right_scope.as_ref().or(outer),
                )?;
                return self.bind_join_output_schema(JoinSchemaBinding {
                    routines,
                    kind: *kind,
                    on: on.as_ref(),
                    using: using.as_ref(),
                    natural: *natural,
                    alias: alias.as_deref(),
                    column_aliases,
                    left: &left_schema,
                    right: &right_schema,
                    subqueries,
                    params,
                    outer,
                });
            }
            SourcePlan::Function {
                name, relations, ..
            } => {
                let lower = crate::semantics::builtin_function_dispatch_name(name);
                if crate::registry::is_operator_join_table_function(&lower) {
                    operator_join_relation_schemas(
                        &self.catalog,
                        &self.resolution,
                        relations.as_ref(),
                    )?;
                    return self.bind_source(routines, source, subqueries, params, outer);
                }
                self.bind_table_function_for_execution(
                    routines, source, subqueries, params, outer,
                )?;
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for function in functions {
                    let mut member = table_function_member_source(function);
                    self.bind_source_for_execution(
                        routines,
                        &mut member,
                        subqueries,
                        params,
                        outer,
                    )?;
                    let SourcePlan::Function { binding, .. } = member else {
                        unreachable!("table-function member changed source kind during binding")
                    };
                    function.binding = binding;
                }
            }
            SourcePlan::Table { .. } | SourcePlan::Values { .. } | SourcePlan::Subquery { .. } => {}
        }
        self.bind_source(routines, source, subqueries, params, outer)
    }

    /// Select a table function's binding against the row it reads. A built-in that reads its input collation also records the collation of its arguments, as analysis records every other call's.
    fn bind_table_function_for_execution(
        &mut self,
        routines: &dyn RoutineResolution,
        source: &mut SourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<(), SQLError> {
        let SourcePlan::Function {
            name,
            binding,
            args,
            ..
        } = source
        else {
            unreachable!("table-function source");
        };
        let input = outer.cloned().unwrap_or_default();
        let resolver = self.query_function_type_resolver_for_subqueries(
            routines, args, &input, subqueries, params,
        )?;
        let Some(mut selected) = crate::semantics::resolve_table_function_binding(
            routines,
            name,
            binding.as_ref(),
            args,
            &input,
            params,
            &resolver,
        )?
        else {
            return Ok(());
        };
        if binding.is_none()
            && selected.builtin
            && crate::expr::binds_input_collation_by_name(
                &crate::semantics::builtin_function_dispatch_name(name),
            )
        {
            selected.input_collation = super::preparation::call_input_collation(
                self, routines, name, args, &input, subqueries, params,
            )?;
        }
        *binding = Some(selected);
        Ok(())
    }
}
