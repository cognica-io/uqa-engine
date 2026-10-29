//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JSON path normalization moves document and path owners while the selected invocation remains borrowed from its admitted result.

use super::{binding, resolve_call, FunctionTypeResolver, JsonPathFunction, RoutineCallDescriptor};
use crate::{
    ast::{FunctionBinding, FunctionDispatch},
    plan::OwnedScalarExpr,
    type_resolution::{
        call::{Arguments, InferType},
        FunctionBindingParts, OwnedBindingCall,
    },
    ColumnType, SQLError, SQLParam, ScalarExpr,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

pub(in crate::type_resolution) fn bind_owned_call(
    call: OwnedBindingCall,
    params: &[SQLParam],
    infer: &mut InferType<'_>,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<OwnedBindingCall, SQLError> {
    control.check()?;
    if JsonPathFunction::lookup(call.name()).is_none()
        || matches!(
            call.binding().and_then(|binding| binding.dispatch),
            Some(FunctionDispatch::JsonPath { .. })
        )
        || call.binding().is_none()
            && resolver
                .map(|resolver| resolver.has_untyped_function_with_control(call.name(), control))
                .transpose()?
                .unwrap_or(false)
    {
        return Ok(call);
    }
    let signature = binding::signature(call.arguments(), params, infer, control)?;
    let selected = match resolve_call(
        call.name(),
        call.binding(),
        RoutineCallDescriptor {
            argument_names: &signature.names,
            argument_types: &signature.effective,
            explicit_variadic: signature.explicit_variadic,
        },
        resolver,
        control,
    ) {
        Ok(Some(selected)) => selected,
        Ok(None) => return Ok(call),
        Err(error) if error.sqlstate() == Some("42883") => {
            return call.undefined(&signature.names, &signature.effective, control)
        }
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
        Err(_) => return Ok(call),
    };
    if !selected.binding.builtin {
        let binding = selected.binding.clone_with_control(control)?;
        drop(selected);
        return call.rebind(binding, control);
    }
    let call = normalize(
        call,
        &selected.binding,
        &signature.original,
        signature.explicit_variadic,
        control,
    )?;
    drop(selected);
    Ok(call)
}

fn normalize(
    mut call: OwnedBindingCall,
    binding: &FunctionBinding,
    original: &[Option<ColumnType>],
    explicit_variadic: bool,
    control: &ProductionControl<'_>,
) -> Result<OwnedBindingCall, SQLError> {
    let entry = JsonPathFunction::lookup(&binding.name).expect("selected JSON path declaration");
    let invocation = binding::invocation(
        binding,
        call.arguments().len(),
        original.len(),
        explicit_variadic,
    )?;
    let mut document = None;
    let mut paths = Arguments::new(call.arguments().len().saturating_sub(1), control)?;
    for (index, original) in original.iter().enumerate() {
        let target =
            ColumnType::from_sql_name_with_control(&invocation.argument_targets[index], control)?;
        let value = crate::scalar_call_argument(&call.arguments()[index])?.value;
        let actual = match value {
            ScalarExpr::TypedLiteral {
                bound_type: Some(ty),
                ..
            } => Some(ty),
            _ => original.as_ref(),
        };
        let needs_cast = actual != Some(&target)
            || matches!(
                value,
                ScalarExpr::Literal(Value::Str(_) | Value::Null) | ScalarExpr::Param(_)
            );
        let (next, value) = call.take_argument(index, control)?;
        call = next;
        let mut value = value.into_call_argument(control)?;
        if needs_cast {
            value = OwnedScalarExpr::cast(
                value,
                target.catalog_name_with_control(control)?,
                true,
                control,
            )?;
        }
        if invocation.argument_positions[index] == 0 {
            document = Some(value);
        } else {
            paths.push(value)?;
        }
    }
    let path = if explicit_variadic {
        paths.finish()?.select_child(0)
    } else {
        paths.finish_array()?
    };
    let document =
        document.ok_or_else(|| SQLError::Internal("JSON path input disappeared".into()))?;
    let arguments = OwnedScalarExpr::boolean([document, path].into_iter(), false, control)?;
    call = call.replace_arguments(arguments, control)?;
    call.rebind(
        normalized_binding(binding, entry.dispatch(), control)?,
        control,
    )
}

fn normalized_binding(
    binding: &FunctionBinding,
    dispatch: FunctionDispatch,
    control: &ProductionControl<'_>,
) -> Result<Produced<FunctionBinding>, SQLError> {
    let mut types = ProductionVec::new(*control);
    types.reserve(binding.argument_types.len())?;
    for name in &binding.argument_types {
        types.push_produced(control.copy_text(name)?)?;
    }
    FunctionBindingParts {
        object_id: binding.object_id,
        name: control.copy_text(&binding.name)?,
        argument_types: types.finish()?,
        builtin: binding.builtin,
        dispatch: Some(dispatch),
        invocation: None,
        value_binding: binding
            .value_binding
            .as_deref()
            .map(|value| crate::plan::copy::copy_syntax_with_control(value, control))
            .transpose()?,
        resolution_error: binding
            .resolution_error
            .as_deref()
            .map(|error| crate::plan::copy::copy_syntax_with_control(error, control))
            .transpose()?,
        input_collation: binding.input_collation,
    }
    .finish(control)
}
