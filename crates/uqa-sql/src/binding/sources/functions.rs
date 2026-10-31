//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FROM result descriptors admit names, selected types and immutable schema storage before allocation.

use crate::{
    ast::{FunctionBinding, OperatorJoinRelations},
    binding::SchemaScope,
    plan::{QueryPlan, SourcePlan},
    routines::RoutineResolution,
    semantics::{ControlledUserTableFunction, TableFunctionTypeRequest},
    type_resolution::FunctionTypeResolver,
    RowSchema, SQLError, SQLParam, ScalarExpr,
};
use uqa_core::memory::{Produced, ProductionVec};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
struct FunctionSource<'a> {
    name: &'a str,
    binding: &'a Option<FunctionBinding>,
    output_name: &'a str,
    relations: &'a Option<OperatorJoinRelations>,
    args: &'a [ScalarExpr],
    alias: &'a Option<String>,
    column_aliases: &'a [String],
    ordinality: &'a bool,
    column_types: &'a [String],
}
impl<'a> FunctionSource<'a> {
    fn new(source: &'a SourcePlan) -> Self {
        let SourcePlan::Function {
            name,
            binding,
            output_name,
            relations,
            args,
            alias,
            column_aliases,
            ordinality,
            column_types,
        } = source
        else {
            unreachable!("function source")
        };
        Self {
            name,
            binding,
            output_name,
            relations,
            args,
            alias,
            column_aliases,
            ordinality,
            column_types,
        }
    }
}
struct FunctionInputs<'a> {
    input: &'a RowSchema,
    subqueries: &'a [QueryPlan],
    params: &'a [SQLParam],
    type_resolver: &'a dyn FunctionTypeResolver,
}

impl SchemaScope {
    pub(in crate::binding) fn bind_function_source(
        &mut self,
        routines: &dyn RoutineResolution,
        source: &SourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        let source = FunctionSource::new(source);
        let FunctionSource {
            name,
            relations,
            args,
            ..
        } = source;
        let resources = self.production.clone();
        let control = resources.control();
        let lower = crate::semantics::builtin_function_dispatch_name_with_control(name, &control)?;
        let operator_join = crate::registry::is_operator_join_table_function(&lower);
        let operator_inputs = operator_join
            .then(|| {
                super::super::catalog_sources::operator_join_relation_schemas_with_control(
                    &self.catalog,
                    &self.resolution,
                    relations.as_ref(),
                    &control,
                )
            })
            .transpose()?;
        let input = match outer {
            Some(outer) => outer.clone(),
            None => RowSchema::empty_with_control(&control)?,
        };
        let type_resolver = self.query_function_type_resolver_for_subqueries(
            routines, args, &input, subqueries, params,
        )?;
        let context = FunctionInputs {
            input: &input,
            subqueries,
            params,
            type_resolver: &type_resolver,
        };
        let user_function =
            self.bind_function_call(routines, source, &context, operator_inputs.as_ref())?;
        self.validate_function_result(routines, source, &context, user_function.as_ref())?;
        let (columns, declared_host_columns) =
            self.function_result_columns(routines, source, user_function.as_ref())?;
        self.function_result_schema(
            routines,
            source,
            &context,
            user_function.as_ref(),
            &columns,
            declared_host_columns,
        )
    }

    fn bind_function_call(
        &mut self,
        routines: &dyn RoutineResolution,
        source: FunctionSource<'_>,
        context: &FunctionInputs<'_>,
        operator_inputs: Option<&(RowSchema, RowSchema)>,
    ) -> Result<Option<ControlledUserTableFunction>, SQLError> {
        let FunctionSource {
            name,
            binding,
            args,
            ..
        } = source;
        let FunctionInputs {
            input,
            subqueries,
            params,
            type_resolver,
        } = *context;
        let resources = self.production.clone();
        let control = resources.control();
        let user_function = if let Some((left, right)) = operator_inputs {
            if self.validate_references {
                let constant = RowSchema::empty_with_control(&control)?;
                for (position, argument) in args.iter().enumerate() {
                    let schema = match position {
                        0 => left,
                        1 => right,
                        _ => &constant,
                    };
                    self.validate_expression_references(
                        routines, argument, schema, None, subqueries, params,
                    )?;
                }
            }
            None
        } else if self.validate_references {
            self.validate_table_function_source(
                routines,
                crate::binding::analysis::TableFunctionSourceValidation {
                    name,
                    binding: binding.as_ref(),
                    args,
                    subqueries,
                    input,
                    params,
                },
            )?
        } else {
            crate::semantics::resolve_user_table_function_with_control(
                crate::semantics::TableFunctionAnalysis {
                    routines,
                    types: type_resolver,
                },
                name,
                binding.as_ref(),
                args,
                input,
                params,
                &control,
            )?
        };
        Ok(user_function)
    }

    fn validate_function_result(
        &mut self,
        routines: &dyn RoutineResolution,
        source: FunctionSource<'_>,
        context: &FunctionInputs<'_>,
        user_function: Option<&ControlledUserTableFunction>,
    ) -> Result<(), SQLError> {
        let FunctionSource {
            name,
            binding,
            args,
            column_types,
            column_aliases,
            ..
        } = source;
        let FunctionInputs {
            input,
            params,
            type_resolver,
            ..
        } = *context;
        let resources = self.production.clone();
        let control = resources.control();
        crate::semantics::validate_table_function_column_definition_with_control(
            routines,
            crate::semantics::ColumnDefinitionRequest {
                name,
                binding: binding.as_ref(),
                user_function: user_function
                    .as_ref()
                    .map(|resolved| resolved.function.as_ref()),
                user_invocation: user_function
                    .as_ref()
                    .and_then(|resolved| resolved.binding.invocation.as_deref()),
                args,
                declared_types: column_types,
                column_names: column_aliases,
            },
            input,
            params,
            type_resolver,
            &control,
        )?;
        if let Some(function) = &user_function {
            self.record_routine_dependency(&function.binding)?;
        }
        Ok(())
    }

    fn function_result_columns(
        &self,
        routines: &dyn RoutineResolution,
        source: FunctionSource<'_>,
        user_function: Option<&ControlledUserTableFunction>,
    ) -> Result<(Produced<Vec<String>>, bool), SQLError> {
        let FunctionSource {
            name,
            output_name,
            alias,
            column_aliases,
            ordinality,
            args,
            ..
        } = source;
        let resources = self.production.clone();
        let control = resources.control();
        let registered_columns = if user_function.is_none() {
            routines.registered_table_function_columns_with_control(name, &control)?
        } else {
            None
        };
        let declared_host_columns = registered_columns.is_some();
        if registered_columns
            .as_ref()
            .is_some_and(|columns| columns.is_empty())
        {
            return Err(SQLError::TypeMismatch(
                control
                    .format(format_args!("table function `{name}` declares no columns"))?
                    .into_parts()
                    .0,
            ));
        }
        let catalog_columns = if !self.validate_references && user_function.is_none() {
            super::super::catalog_sources::user_function_output_columns_with_control(
                self.catalog.as_ref(),
                &self.resolution,
                name,
                &control,
            )?
        } else {
            None
        };
        let columns = if let Some(resolved) = &user_function {
            crate::semantics::user_table_function_columns_with_control(
                routines,
                &resolved.function,
                resolved.binding.invocation.as_deref(),
                output_name,
                alias.as_deref(),
                column_aliases,
                *ordinality,
                &control,
            )?
        } else {
            match registered_columns.or(catalog_columns) {
                Some(columns) => crate::semantics::apply_table_function_aliases_with_control(
                    &columns,
                    column_aliases,
                    *ordinality,
                    &control,
                )?,
                None => crate::semantics::table_function_empty_schema_with_control(
                    name,
                    output_name,
                    alias.as_deref(),
                    column_aliases,
                    args.len(),
                    *ordinality,
                    &control,
                )?,
            }
        };
        crate::semantics::validate_table_function_alias_count_with_control(
            alias.as_deref().unwrap_or(output_name),
            columns.len(),
            column_aliases.len(),
            &control,
        )?;
        Ok((columns, declared_host_columns))
    }

    fn function_result_schema(
        &self,
        routines: &dyn RoutineResolution,
        source: FunctionSource<'_>,
        context: &FunctionInputs<'_>,
        user_function: Option<&ControlledUserTableFunction>,
        columns: &[String],
        declared_host_columns: bool,
    ) -> Result<RowSchema, SQLError> {
        let FunctionSource {
            name,
            binding,
            args,
            column_types,
            ordinality,
            alias,
            output_name,
            ..
        } = source;
        let FunctionInputs {
            input,
            params,
            type_resolver,
            ..
        } = *context;
        let resources = self.production.clone();
        let control = resources.control();
        let types = crate::semantics::table_function_column_types_with_control(
            routines,
            TableFunctionTypeRequest {
                name,
                binding: binding.as_ref(),
                args,
                user_function: user_function
                    .as_ref()
                    .map(|resolved| resolved.function.as_ref()),
                user_invocation: user_function
                    .as_ref()
                    .and_then(|resolved| resolved.binding.invocation.as_deref()),
                declared_types: column_types,
                columns,
                ordinality: *ordinality,
            },
            input,
            params,
            type_resolver,
            &control,
        )?;
        let qualifier = alias.as_deref().unwrap_or(output_name);
        let schema = RowSchema::with_column_metadata_with_control(
            Some(qualifier),
            columns
                .iter()
                .zip(types.iter())
                .map(|(name, ty)| (name.as_str(), ty.as_ref(), None)),
            &control,
        )?;
        Ok(
            if user_function.is_none()
                && !declared_host_columns
                && column_types.is_empty()
                && routines.has_registered_table_function_with_control(name, &control)?
            {
                RowSchema::with_open_columns_with_control(&schema, Some(qualifier), &control)?
            } else {
                schema
            },
        )
    }

    pub(in crate::binding) fn bind_function_group_source(
        &mut self,
        routines: &dyn RoutineResolution,
        source: &SourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        let SourcePlan::FunctionGroup {
            functions,
            alias,
            column_aliases,
            ordinality,
        } = source
        else {
            unreachable!("function group")
        };
        let resources = self.production.clone();
        let control = resources.control();
        let first = functions
            .first()
            .ok_or_else(|| SQLError::Internal("ROWS FROM group has no functions".into()))?;
        let mut columns = ProductionVec::new(control);
        let mut types = ProductionVec::new(control);
        let mut open = false;
        for function in functions {
            let member = function.copy_for_execution(&control)?;
            let schema = self.bind_source(routines, &member, subqueries, params, outer)?;
            open |= schema.columns_are_open(None);
            for (position, column) in schema.iter().enumerate() {
                columns.push_produced(
                    control.copy_text(schema.public_name(position).unwrap_or(column))?,
                )?;
            }
            for ty in schema.column_types() {
                types.push_produced(if open {
                    control.finish(None, control.empty_reservation())?
                } else {
                    crate::plan::copy::copy_syntax_with_control(ty, &control)?
                })?;
            }
        }
        if *ordinality {
            columns.push_produced(control.copy_text("ordinality")?)?;
            types.push_produced(control.finish(
                (!open).then_some(crate::ColumnType::BigInteger),
                control.empty_reservation(),
            )?)?;
        }
        let qualifier = alias.as_deref().unwrap_or(&first.output_name);
        if !open {
            crate::semantics::validate_table_function_alias_count_with_control(
                qualifier,
                columns.len(),
                column_aliases.len(),
                &control,
            )?;
        }
        let schema = RowSchema::with_column_metadata_with_control(
            Some(qualifier),
            columns
                .iter()
                .zip(types.iter())
                .enumerate()
                .map(|(position, (name, ty))| {
                    (
                        column_aliases.get(position).unwrap_or(name).as_str(),
                        ty.as_ref(),
                        None,
                    )
                }),
            &control,
        )?;
        if open {
            RowSchema::with_open_columns_with_control(&schema, Some(qualifier), &control)
        } else {
            Ok(schema)
        }
    }
}
