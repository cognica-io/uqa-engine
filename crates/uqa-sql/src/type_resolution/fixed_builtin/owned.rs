//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed-call normalization reuses catalog matching while transferring each supplied scalar owner exactly once.

use super::{
    binding::{self, Signature},
    registry, selected,
};
use crate::{
    ast::{FunctionBinding, FunctionDispatch},
    plan::OwnedScalarExpr,
    type_resolution::{
        call::{Arguments, InferType},
        FunctionBindingParts, FunctionTypeResolver, OwnedBindingCall,
    },
    SQLError, SQLParam,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(in crate::type_resolution) fn bind_owned_call(
    call: OwnedBindingCall,
    params: &[SQLParam],
    infer: &mut InferType<'_>,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<OwnedBindingCall, SQLError> {
    control.check()?;
    if matches!(
        call.binding().and_then(|binding| binding.dispatch),
        Some(FunctionDispatch::Range { .. })
    ) || call.binding().is_none()
        && resolver
            .map(|resolver| resolver.has_untyped_function_with_control(call.name(), control))
            .transpose()?
            .unwrap_or(false)
    {
        return Ok(call);
    }
    let Some((registered, _)) = registry::lookup(call.name()) else {
        return Ok(call);
    };
    let signature = match binding::signature(call.arguments(), params, infer, control) {
        Ok(signature) => signature,
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
        Err(_) => return Ok(call),
    };
    let mut call = call;
    if resolver.is_none()
        && !(signature.explicit_variadic && signature.names.iter().any(Option::is_some))
    {
        if let Some(binding) = call.binding().filter(|binding| {
            registry::lookup(&binding.name).is_some_and(|(name, _)| name == registered)
        }) {
            let binding = binding.clone_with_control(control)?;
            let (updated, matched) = normalize(call, &binding, &signature, None, control)?;
            call = updated;
            if matched {
                return Ok(call);
            }
        }
    }
    let selected = match super::resolve_overload_with_control(
        call.name(),
        call.binding(),
        &signature.names,
        &signature.effective,
        signature.explicit_variadic,
        resolver,
        control,
    ) {
        Ok(selected) => selected,
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
    let infer_again = resolver.map(|_| infer);
    let (call, _) = normalize(call, &selected.binding, &signature, infer_again, control)?;
    drop(selected);
    Ok(call)
}

fn normalize(
    mut call: OwnedBindingCall,
    binding: &FunctionBinding,
    observed: &Signature,
    infer_again: Option<&mut InferType<'_>>,
    control: &ProductionControl<'_>,
) -> Result<(OwnedBindingCall, bool), SQLError> {
    let Some(signature) = registry::bound_signature(binding, control)? else {
        return Ok((call, false));
    };
    let Some(matched) = crate::type_resolution::overload_resolution::match_signature_with_control(
        signature,
        &observed.names,
        &observed.effective,
        control,
    )?
    else {
        return Ok((call, false));
    };
    super::validate_polymorphic_arguments(
        signature.argument_types,
        &observed.effective,
        &matched.argument_positions,
    )?;
    if call.arguments().len() != observed.original.len()
        || !selected::valid_positions(
            &matched.argument_positions,
            call.arguments().len(),
            signature.argument_types.len(),
            &binding.name,
        )
    {
        return Ok((call, false));
    }
    let mut arguments = Arguments::new(signature.argument_types.len(), control)?;
    for (position, declared) in signature.argument_types.iter().enumerate() {
        control.check()?;
        let supplied = matched
            .argument_positions
            .iter()
            .position(|&slot| slot == position);
        let cast = if infer_again.is_none() {
            supplied
                .map(|index| {
                    selected::requires_cast(
                        super::named_argument_value(&call.arguments()[index]),
                        observed.original[index].as_ref(),
                        declared,
                        control,
                    )
                })
                .transpose()?
                .unwrap_or(false)
        } else {
            false
        };
        let mut argument = if let Some(index) = supplied {
            let (updated, argument) = call.take_argument(index, control)?;
            call = updated;
            argument.into_call_argument(control)?
        } else {
            super::default_argument(&binding.name, position)
                .expect("missing fixed arguments were prevalidated")
                .copy_for_rewrite(control)?
        };
        if cast {
            argument = OwnedScalarExpr::cast(
                argument,
                declared.catalog_name_with_control(control)?,
                true,
                control,
            )?;
        }
        arguments.push(argument)?;
    }
    call = call.replace_arguments(arguments.finish()?, control)?;
    if let Some(infer) = infer_again {
        call = coerce_normalized(call, signature.argument_types, infer, control)?;
    }
    Ok((
        finish_binding(call, binding, signature.argument_types, control)?,
        true,
    ))
}

fn coerce_normalized(
    mut call: OwnedBindingCall,
    argument_types: &[crate::ColumnType],
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<OwnedBindingCall, SQLError> {
    for (position, declared) in argument_types.iter().enumerate() {
        let actual = match crate::type_resolution::call::infer_with_control(
            &call.arguments()[position],
            infer,
            control,
        ) {
            Ok(actual) => actual,
            Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
            Err(_) => None,
        };
        if selected::requires_cast(
            &call.arguments()[position],
            actual.as_deref(),
            declared,
            control,
        )? {
            call = call.map_argument(position, control, |argument| {
                Ok(OwnedScalarExpr::cast(
                    argument,
                    declared.catalog_name_with_control(control)?,
                    true,
                    control,
                )?)
            })?;
        }
    }
    Ok(call)
}

fn finish_binding(
    call: OwnedBindingCall,
    binding: &FunctionBinding,
    argument_types: &[crate::ColumnType],
    control: &ProductionControl<'_>,
) -> Result<OwnedBindingCall, SQLError> {
    let selected_dispatch =
        call.binding()
            .and_then(|binding| binding.dispatch)
            .filter(|dispatch| {
                matches!(
                    dispatch,
                    FunctionDispatch::TypeOf { .. } | FunctionDispatch::CollationFor { .. }
                )
            });
    let mut types = ProductionVec::new(*control);
    types.reserve(argument_types.len())?;
    for ty in argument_types {
        types.push_produced(ty.sql_name_with_control(control)?)?;
    }
    let (name, _) = registry::lookup(&binding.name).expect("selected fixed registry descriptor");
    let binding = FunctionBindingParts {
        object_id: None,
        name: control.format(format_args!("pg_catalog.{name}"))?,
        argument_types: types.finish()?,
        builtin: true,
        dispatch: selected_dispatch.or(binding.dispatch),
        invocation: None,
        value_binding: binding
            .value_binding
            .as_deref()
            .map(|value| crate::plan::copy::copy_syntax_with_control(value, control))
            .transpose()?,
        resolution_error: None,
        input_collation: binding.input_collation,
    }
    .finish(control)?;
    let binding = runtime_binding(binding, control)?;
    call.rebind(binding, control)
}

fn runtime_binding(
    binding: Produced<FunctionBinding>,
    control: &ProductionControl<'_>,
) -> Result<Produced<FunctionBinding>, SQLError> {
    let (mut binding, memory) = binding.into_parts();
    binding.dispatch = super::runtime_dispatch(&binding);
    Ok(control.finish(binding, memory)?)
}
