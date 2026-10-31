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
    operator_join_relation_schemas, projection::rename_schema_with_control, BindingContext,
    ScalarExpr, SchemaScope, SourceScope,
};
use crate::routines::RoutineResolution;

mod execution;
mod functions;
mod relations;
pub use execution::bind_source_plan_for_execution_owned;

pub(super) fn alias_table_schema(
    schema: &RowSchema,
    qualifier: &str,
    column_aliases: &[String],
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    if column_aliases.len() > schema.len() {
        return Err(SQLError::Routine {
            sqlstate: "42P10".into(),
            message: control
                .format(format_args!(
                    "table \"{qualifier}\" has {} columns available but {} columns specified",
                    schema.len(),
                    column_aliases.len()
                ))?
                .into_parts()
                .0,
        });
    }
    rename_schema_with_control(schema, column_aliases, Some(qualifier), control)
}

impl SchemaScope {
    pub(super) fn alias_relation_schema(
        &self,
        schema: &RowSchema,
        name: &str,
        qualifier: &str,
        column_aliases: &[String],
    ) -> Result<RowSchema, SQLError> {
        let control = self.control();
        let schema = alias_table_schema(schema, qualifier, column_aliases, &control)?;
        match self
            .catalog
            .relation_row_type_with_control(&self.resolution, name, &control)?
        {
            Some(row_type) => {
                let (row_type, memory) = row_type.into_parts();
                let ty = control.finish(crate::ColumnType::Composite(row_type), memory)?;
                schema.with_relation_row_type_with_control(qualifier, &ty, &control)
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

/// Inputs already visited by an ordered source traversal. Join binding consumes the child descriptors without analyzing their sources again.
#[doc(hidden)]
pub enum SourceBindingInput<'a> {
    Leaf(&'a SourcePlan),
    Join {
        source: &'a SourcePlan,
        left: &'a RowSchema,
        right: &'a RowSchema,
    },
}

impl<'a> SourceBindingInput<'a> {
    pub fn source(&self) -> &'a SourcePlan {
        match self {
            Self::Leaf(source) | Self::Join { source, .. } => source,
        }
    }
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

/// Bind a FROM item while retaining the distinction between lexical ancestors and preceding peers for consumers which traverse a source tree.
pub fn bind_source_plan_schema_in_scope(
    routines: &dyn RoutineResolution,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &BindingContext,
    scope: SourceScope<'_>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::from_context(ctes)?.bind_source_in_scope(
        routines,
        source,
        ctes.scalar_subqueries,
        params,
        scope,
    )
}

/// Bind one visited source using its already analyzed child descriptors.
pub fn bind_source_input_schema(
    routines: &dyn RoutineResolution,
    input: SourceBindingInput<'_>,
    params: &[SQLParam],
    ctes: &BindingContext,
    scope: SourceScope<'_>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::from_context(ctes)?.bind_source_input(
        routines,
        input,
        ctes.scalar_subqueries,
        params,
        scope,
    )
}

mod pseudo_columns;
pub use pseudo_columns::with_query_table_pseudo_columns_with_control;

/// Add query-block pseudo columns after the complete source scope is known, so `_meta` is exposed only for one unambiguous local-table source and never shadows a real relation alias.
pub fn with_query_table_pseudo_columns(schema: &RowSchema) -> RowSchema {
    with_query_table_pseudo_columns_with_control(
        schema,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary pseudo-column schema")
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
    pub(super) fn bind_source_input(
        &mut self,
        routines: &dyn RoutineResolution,
        input: SourceBindingInput<'_>,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        scope: SourceScope<'_>,
    ) -> Result<RowSchema, SQLError> {
        let SourceBindingInput::Join {
            source,
            left,
            right,
        } = input
        else {
            return self.bind_source_in_scope(routines, input.source(), subqueries, params, scope);
        };
        let SourcePlan::Join {
            kind,
            on,
            using,
            natural,
            alias,
            column_aliases,
            ..
        } = source
        else {
            unreachable!("join inputs belong to a join source")
        };
        let condition = scope.condition_with_control(&self.control())?;
        let schema = self.bind_join_output_schema(JoinSchemaBinding {
            routines,
            kind: *kind,
            on: on.as_ref(),
            using: using.as_ref(),
            natural: *natural,
            alias: alias.as_deref(),
            column_aliases,
            left,
            right,
            subqueries,
            params,
            outer: condition.as_ref(),
        })?;
        Ok(if scope.retain_names {
            SourceScope::describe_output_with_control(source, schema, &self.control())?
        } else {
            schema
        })
    }

    pub(super) fn bind_source(
        &mut self,
        routines: &dyn RoutineResolution,
        source: &SourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        self.bind_source_in_scope(
            routines,
            source,
            subqueries,
            params,
            SourceScope::new(outer),
        )
    }

    pub(super) fn bind_source_in_scope(
        &mut self,
        routines: &dyn RoutineResolution,
        source: &SourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        scope: SourceScope<'_>,
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
        let result = self.bind_source_inner(routines, source, subqueries, params, scope);
        if let Some(previous) = previous {
            self.resolution.set_lookup_mode(previous);
        }
        result.and_then(|schema| {
            if scope.retain_names {
                SourceScope::describe_output_with_control(source, schema, &self.control())
            } else {
                Ok(schema)
            }
        })
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
        self.bind_source_for_execution_in_scope(
            routines,
            source,
            subqueries,
            params,
            SourceScope::new(outer),
        )
    }

    fn bind_source_for_execution_in_scope(
        &mut self,
        routines: &dyn RoutineResolution,
        source: &mut SourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        scope: SourceScope<'_>,
    ) -> Result<RowSchema, SQLError> {
        let schema = self.bind_source_for_execution_in_scope_inner(
            routines, source, subqueries, params, scope,
        )?;
        Ok(if scope.retain_names {
            SourceScope::describe_output_with_control(source, schema, &self.control())?
        } else {
            schema
        })
    }

    fn bind_source_for_execution_in_scope_inner(
        &mut self,
        routines: &dyn RoutineResolution,
        source: &mut SourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        scope: SourceScope<'_>,
    ) -> Result<RowSchema, SQLError> {
        let input = (!matches!(source, SourcePlan::Join { .. }))
            .then(|| scope.input_with_control(source, &self.control()))
            .transpose()?
            .flatten();
        let outer = input.as_ref();
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
                let left_schema = self.bind_source_for_execution_in_scope(
                    routines,
                    left,
                    subqueries,
                    params,
                    scope.with_names(),
                )?;
                let preceding = scope.right_namespace_with_control(
                    left,
                    &left_schema,
                    *kind,
                    &self.control(),
                )?;
                let right_schema = self.bind_source_for_execution_in_scope(
                    routines,
                    right,
                    subqueries,
                    params,
                    SourceScope {
                        outer: scope.outer,
                        preceding: Some(&preceding),
                        inherited_lateral: *lateral,
                        retain_names: true,
                    },
                )?;
                let condition = scope.condition_with_control(&self.control())?;
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
                    outer: condition.as_ref(),
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
        let selected =
            self.select_execution_table_function(routines, source, subqueries, params, outer)?;
        if let Some(selected) = selected {
            let SourcePlan::Function { binding, .. } = source else {
                unreachable!("table-function source")
            };
            *binding = Some(
                selected
                    .into_uncontrolled()
                    .expect("ordinary execution binding"),
            );
        }
        Ok(())
    }

    fn select_execution_table_function(
        &mut self,
        routines: &dyn RoutineResolution,
        source: &SourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<Option<uqa_core::memory::Produced<crate::ast::FunctionBinding>>, SQLError> {
        let resources = self.production.clone();
        let control = resources.control();
        let SourcePlan::Function {
            name,
            binding,
            args,
            ..
        } = source
        else {
            unreachable!("table-function source")
        };
        let input = outer.cloned().unwrap_or_default();
        let resolver = self.query_function_type_resolver_for_subqueries(
            routines, args, &input, subqueries, params,
        )?;
        let Some(selected) = crate::semantics::resolve_table_function_binding_with_control(
            crate::semantics::TableFunctionAnalysis {
                routines,
                types: &resolver,
            },
            name,
            binding.as_ref(),
            args,
            &input,
            params,
            &control,
        )?
        else {
            return Ok(None);
        };
        let (mut selected, memory) = selected.into_parts();
        if binding.is_none()
            && selected.builtin
            && crate::expr::binds_input_collation_by_name(
                &crate::semantics::builtin_function_dispatch_name_with_control(name, &control)?,
            )
        {
            selected.input_collation = super::preparation::call_input_collation(
                self, routines, name, args, &input, subqueries, params,
            )?;
        }
        Ok(Some(control.finish(selected, memory)?))
    }
}
