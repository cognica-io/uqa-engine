//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JSON path normalization moves document and path owners while the selected invocation remains borrowed from its admitted result.

use super::{binding, resolve_call, FunctionTypeResolver, JsonPathFunction, RoutineCallDescriptor};
use crate::{
    ast::{FunctionBinding, FunctionDispatch},
    plan::{
        retirement::{checkpoint, retire_produced_from},
        OwnedScalarExpr, ScalarInputWork as Work,
    },
    schema::retention::CatalogRetentionError,
    type_resolution::{
        call::{Arguments, InferType},
        OwnedBindingCall,
    },
    ColumnType, SQLError, SQLParam, ScalarExpr,
};
use uqa_core::{
    memory::{ProductionControl, ProductionVec},
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

/// Ordered preparation selected the invocation already; normalize its owned children without repeating overload resolution.
pub(crate) fn normalize_retained_owned(
    expression: OwnedScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    Ok(crate::catalog::work::inline(
        normalize_retained_owned_from(expression, control, &mut ()),
    )?)
}

pub(crate) async fn normalize_retained_owned_from<W: Work>(
    expression: OwnedScalarExpr,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<OwnedScalarExpr, W::Error>
where
    W::Error: From<SQLError> + Into<SQLError> + From<CatalogRetentionError>,
{
    let ScalarExpr::Func {
        binding: Some(binding),
        ..
    } = &*expression
    else {
        return Ok(expression);
    };
    if !binding.builtin
        || JsonPathFunction::lookup(&binding.name).is_none()
        || matches!(binding.dispatch, Some(FunctionDispatch::JsonPath { .. }))
        || binding.invocation.is_none()
    {
        return Ok(expression);
    }
    let binding = binding.clone_with_input_work(control, work).await?;
    let invocation = binding
        .invocation
        .as_ref()
        .expect("selected JSON path invocation");
    let mut original = ProductionVec::new(*control);
    original.reserve(invocation.argument_sources.len())?;
    for source in &invocation.argument_sources {
        checkpoint(control, work).await?;
        let parsed = match source.as_deref() {
            Some(source) => match work
                .parse_type(source, control)
                .await
                .map_err(Into::<SQLError>::into)
            {
                Ok(value) => Some(value),
                Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                    return Err(error.into())
                }
                Err(_) => None,
            },
            None => None,
        };
        let (value, memory) = parsed.map_or((None, control.empty_reservation()), |value| {
            let (value, memory) = value.into_parts();
            (Some(value), memory)
        });
        original.push_produced(control.finish(value, memory)?)?;
    }
    let original = original.finish()?;
    let explicit = matches!(
        invocation.variadic_mode,
        crate::ast::RoutineVariadicMode::Explicit { .. }
    );
    let output = normalize_from(
        OwnedBindingCall::new(expression, control).map_err(W::Error::from)?,
        &binding,
        &original,
        explicit,
        control,
        work,
    )
    .await?
    .into_expression();
    retire_produced_from(original, control, work).await?;
    retire_produced_from(binding, control, work).await?;
    Ok(output)
}

fn normalize(
    call: OwnedBindingCall,
    binding: &FunctionBinding,
    original: &[Option<ColumnType>],
    explicit_variadic: bool,
    control: &ProductionControl<'_>,
) -> Result<OwnedBindingCall, SQLError> {
    Ok(crate::catalog::work::inline(normalize_from(
        call,
        binding,
        original,
        explicit_variadic,
        control,
        &mut (),
    ))?)
}

async fn normalize_from<W: Work>(
    mut call: OwnedBindingCall,
    binding: &FunctionBinding,
    original: &[Option<ColumnType>],
    explicit_variadic: bool,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<OwnedBindingCall, W::Error>
where
    W::Error: From<SQLError> + Into<SQLError> + From<CatalogRetentionError>,
{
    let entry = JsonPathFunction::lookup(&binding.name).expect("selected JSON path declaration");
    let invocation = binding::invocation_from(
        binding,
        call.arguments().len(),
        original.len(),
        explicit_variadic,
        control,
        work,
    )
    .await?;
    let mut document = None;
    let mut paths = Arguments::new(call.arguments().len().saturating_sub(1), control)
        .map_err(W::Error::from)?;
    for (index, original) in original.iter().enumerate() {
        checkpoint(control, work).await?;
        let target = work
            .parse_type(&invocation.argument_targets[index], control)
            .await?;
        let value = crate::scalar_call_argument(&call.arguments()[index])
            .map_err(W::Error::from)?
            .value;
        let actual = match value {
            ScalarExpr::TypedLiteral {
                bound_type: Some(ty),
                ..
            } => Some(ty),
            _ => original.as_ref(),
        };
        let needs_cast = !matches_target(actual, &target, control, work).await?
            || matches!(
                value,
                ScalarExpr::Literal(Value::Str(_) | Value::Null) | ScalarExpr::Param(_)
            );
        let (next, value) = call.take_argument(index, control).map_err(W::Error::from)?;
        call = next;
        let mut value = value.into_call_argument_from(control, work).await?;
        if needs_cast {
            value = OwnedScalarExpr::cast(
                value,
                work.type_name(&target, control).await?,
                true,
                control,
            )?;
        }
        retire_produced_from(target, control, work).await?;
        if invocation.argument_positions[index] == 0 {
            document = Some(value);
        } else {
            paths.push(value).map_err(W::Error::from)?;
        }
    }
    let path = if explicit_variadic {
        paths
            .finish_from(false, work)
            .await?
            .select_child_from(0, control, work)
            .await?
    } else {
        paths.finish_from(true, work).await?
    };
    let document = document
        .ok_or_else(|| W::Error::from(SQLError::Internal("JSON path input disappeared".into())))?;
    let arguments =
        OwnedScalarExpr::boolean_from([document, path].into_iter(), false, control, work).await?;
    let binding = binding
        .normalized_with_input_work(entry.dispatch(), control, work)
        .await?;
    call.replace_from(arguments, binding, control, work).await
}

// Parsed SQL targets contain only array wrappers and built-in scalar leaves. Reuse scalar equality after walking those wrappers without recursive comparison.
async fn matches_target<W: Work>(
    actual: Option<&ColumnType>,
    mut target: &ColumnType,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<bool, W::Error> {
    let Some(mut actual) = actual else {
        return Ok(false);
    };
    while let ColumnType::Array(target_child) = target {
        checkpoint(control, work).await?;
        let ColumnType::Array(actual_child) = actual else {
            return Ok(false);
        };
        actual = actual_child;
        target = target_child;
    }
    Ok(actual == target)
}
