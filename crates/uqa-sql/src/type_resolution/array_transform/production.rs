//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Array-transform overload precedence keeps builtin and catalog candidate owners distinct.

use super::{
    ambiguous_function, named_argument_name, named_argument_value,
    resolve_builtin_type_with_control, undefined_function, user_argument_types, ColumnType,
    FunctionBinding, FunctionDispatch, FunctionTypeResolver, Produced, ProductionControl,
    ProductionVec, ResolvedFunctionOverload, SQLError, ScalarExpr, Value,
};

pub(super) enum OwnedSelection {
    Builtin(Produced<ColumnType>),
    User(Produced<ResolvedFunctionOverload>),
}

pub(in crate::type_resolution) fn resolve_type_with_resolver_and_control(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    match select_overload_with_control(
        name,
        binding,
        args,
        argument_types,
        explicit_variadic,
        resolver,
        control,
    )? {
        OwnedSelection::Builtin(ty) => Ok(Some(ty)),
        OwnedSelection::User(selected) => selected
            .return_type
            .clone_with_control(control)
            .map(Some)
            .map_err(Into::into),
    }
}

pub(super) fn select_overload_with_control(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<OwnedSelection, SQLError> {
    control.check()?;
    if binding.and_then(|binding| binding.dispatch) == Some(FunctionDispatch::ArraySortJson) {
        return resolve_builtin_type_with_control(name, args, argument_types, control)
            .map(OwnedSelection::Builtin);
    }
    let builtin = if explicit_variadic
        && args
            .iter()
            .any(|argument| named_argument_name(argument).is_some())
    {
        Err(undefined_function(name, args, argument_types))
    } else {
        resolve_builtin_type_with_control(name, args, argument_types, control)
    };
    let builtin = match builtin {
        Err(error)
            if matches!(error.primary(), SQLError::Cancelled(_))
                || error.sqlstate() == Some("53200") =>
        {
            return Err(error)
        }
        other => other,
    };
    let user = match user_overload(
        name,
        binding,
        args,
        argument_types,
        explicit_variadic,
        resolver,
        control,
    ) {
        Err(error) if binding.is_none() && error.sqlstate() == Some("42883") => None,
        other => other?,
    };
    if binding.is_some() {
        return user.map(OwnedSelection::User).ok_or_else(|| {
            builtin.err().unwrap_or_else(|| {
                undefined_function(name, args, &user_argument_types(args, argument_types))
            })
        });
    }
    match (builtin, user) {
        (Ok(return_type), None) => Ok(OwnedSelection::Builtin(return_type)),
        (Ok(_), Some(user)) if user.is_exact_for_known_arguments() => {
            Ok(OwnedSelection::User(user))
        }
        (Ok(_), Some(_)) => Err(ambiguous_function(name, args, argument_types)),
        (Err(_), Some(user)) => Ok(OwnedSelection::User(user)),
        (Err(error), None) => Err(error),
    }
}

fn user_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ResolvedFunctionOverload>>, SQLError> {
    if binding.is_none()
        && name
            .get(.."pg_catalog.".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("pg_catalog."))
    {
        return Ok(None);
    }
    let Some(resolver) = resolver else {
        return Ok(None);
    };
    let mut names = ProductionVec::new(*control);
    let mut types = ProductionVec::new(*control);
    names.reserve(args.len())?;
    types.reserve(argument_types.len())?;
    for argument in args {
        let name = named_argument_name(argument)
            .map(|name| control.copy_text(name))
            .transpose()?;
        let (name, memory) = name.map_or((None, control.empty_reservation()), |name| {
            let (name, memory) = name.into_parts();
            (Some(name), memory)
        });
        names.push_produced(control.finish(name, memory)?)?;
    }
    for (argument, ty) in args.iter().zip(argument_types) {
        let ty = if matches!(
            named_argument_value(argument),
            ScalarExpr::Literal(Value::Str(_) | Value::Null)
        ) {
            None
        } else {
            ty.as_ref()
        };
        let ty = ty.map(|ty| ty.clone_with_control(control)).transpose()?;
        let (ty, memory) = ty.map_or((None, control.empty_reservation()), |ty| {
            let (ty, memory) = ty.into_parts();
            (Some(ty), memory)
        });
        types.push_produced(control.finish(ty, memory)?)?;
    }
    let names = names.finish()?;
    let types = types.finish()?;
    resolver.resolve_function_overload_with_control(
        name,
        binding,
        &names,
        &types,
        explicit_variadic,
        control,
    )
}
