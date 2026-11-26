//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL function range entries use the call's result collation for scalar output and the declaration's attribute collations for named tuples.

use super::{
    FunctionInputs, RoutineResolution, RowSchema, SQLError, SQLParam, ScalarExpr, SchemaScope,
    TableFunctionTypeRequest,
};
use crate::binding::production::ControlledBindingContext;
use crate::catalog::collations::CollationSelection;
use crate::semantics::TableFunctionAnalysis;
use uqa_core::memory::{ProductionControl, ProductionVec};

/// Describe a physical function scan using the range-entry binder. Capture its lexical namespace only for arguments whose collation was not retained during analysis.
pub fn bind_table_function_result_schema<'a>(
    analysis: TableFunctionAnalysis<'_>,
    request: TableFunctionTypeRequest<'_>,
    input: &RowSchema,
    params: &[SQLParam],
    binding: impl Fn() -> Result<ControlledBindingContext<'a>, SQLError>,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    let routines = analysis.routines;
    let mut scope = None;
    result_schema(analysis, request, input, params, None, control, |args| {
        let (scope, subqueries) = match &mut scope {
            Some(state) => state,
            empty @ None => {
                let binding = binding()?;
                let subqueries = binding.scalar_subqueries;
                empty.insert((
                    SchemaScope::for_analysis_with_control(binding, control)?,
                    subqueries,
                ))
            }
        };
        crate::binding::preparation::call_input_collation(
            scope,
            routines,
            request.name,
            args,
            input,
            subqueries,
            params,
        )
    })
}

impl SchemaScope {
    pub(super) fn table_function_result(
        &mut self,
        routines: &dyn RoutineResolution,
        request: TableFunctionTypeRequest<'_>,
        context: &FunctionInputs<'_>,
        qualifier: Option<&str>,
    ) -> Result<RowSchema, SQLError> {
        let resources = self.production.clone();
        result_schema(
            TableFunctionAnalysis {
                routines,
                types: context.type_resolver,
            },
            request,
            context.input,
            context.params,
            qualifier,
            &resources.control(),
            |args| {
                crate::binding::preparation::call_input_collation(
                    self,
                    routines,
                    request.name,
                    args,
                    context.input,
                    context.subqueries,
                    context.params,
                )
            },
        )
    }
}

fn result_schema(
    analysis: TableFunctionAnalysis<'_>,
    request: TableFunctionTypeRequest<'_>,
    input: &RowSchema,
    params: &[SQLParam],
    qualifier: Option<&str>,
    control: &ProductionControl<'_>,
    mut derive: impl FnMut(&[ScalarExpr]) -> Result<Option<CollationSelection>, SQLError>,
) -> Result<RowSchema, SQLError> {
    let columns =
        crate::semantics::table_function_columns(analysis, request, input, params, control)?;
    let width = columns
        .types
        .len()
        .saturating_sub(usize::from(request.ordinality));
    let mut collations = ProductionVec::new(*control);
    if request.declared_types.is_empty()
        && columns.tuple.is_none()
        && columns
            .types
            .iter()
            .flatten()
            .any(crate::schema::composites::type_is_collatable)
    {
        if width == 1 {
            let selected = match request.binding.and_then(|binding| binding.input_collation) {
                Some(bound) => Some(bound),
                None => derive(request.args)?,
            };
            collations.push_copy(selected.map(CollationSelection::oid))?;
        } else if request.user_function.is_none()
            && width == request.args.len()
            && crate::semantics::builtin_function_dispatch_name_with_control(request.name, control)?
                .as_str()
                == "unnest"
        {
            for argument in request.args {
                collations.push_copy(
                    derive(std::slice::from_ref(argument))?.map(CollationSelection::oid),
                )?;
            }
        }
    }
    RowSchema::with_column_metadata_with_control(
        qualifier,
        request
            .columns
            .iter()
            .zip(columns.types.iter())
            .enumerate()
            .map(|(index, (name, ty))| {
                let collation = match &columns.tuple {
                    Some(tuple) => tuple
                        .attributes
                        .get(index)
                        .and_then(|attr| attr.collation_oid),
                    None => collations.get(index).copied().flatten(),
                };
                (name.as_str(), ty.as_ref(), collation)
            }),
        control,
    )
}
