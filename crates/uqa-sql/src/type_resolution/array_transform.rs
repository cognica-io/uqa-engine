//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible binding for `array_sort` and `array_reverse`.

use super::call::{BindingCall, InferType};
use super::common::base_type;
use super::functions::named_argument_value;
use super::{FunctionTypeResolver, ResolvedFunctionOverload};
use crate::ast::{ColumnType, FunctionBinding, FunctionDispatch};
use crate::{scalar_call_arguments, schema::ScalarTypeSchema, ScalarExpr};
use crate::{SQLError, SQLParam};
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionControl, ProductionVec},
    Value,
};

pub(super) fn resolve_type(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    select_overload(
        name,
        binding,
        args,
        argument_types,
        explicit_variadic,
        resolver,
    )
    .map(|selected| {
        Some(match selected {
            SelectedOverload::Builtin(return_type) => return_type,
            SelectedOverload::User(overload) => overload.return_type,
        })
    })
}

enum SelectedOverload {
    Builtin(ColumnType),
    User(ResolvedFunctionOverload),
}

fn select_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<SelectedOverload, SQLError> {
    if binding.and_then(|binding| binding.dispatch) == Some(FunctionDispatch::ArraySortJson) {
        return resolve_builtin_type(name, args, argument_types).map(SelectedOverload::Builtin);
    }
    let builtin = if explicit_variadic
        && args
            .iter()
            .any(|argument| named_argument_name(argument).is_some())
    {
        Err(undefined_function(name, args, argument_types))
    } else {
        resolve_builtin_type(name, args, argument_types)
    };
    let user = match resolve_user_overload(
        name,
        binding,
        args,
        argument_types,
        explicit_variadic,
        resolver,
    ) {
        Err(error) if binding.is_none() && error.sqlstate() == Some("42883") => None,
        other => other?,
    };
    if binding.is_some() {
        return user.map(SelectedOverload::User).ok_or_else(|| {
            builtin.err().unwrap_or_else(|| {
                undefined_function(name, args, &user_argument_types(args, argument_types))
            })
        });
    }
    match (builtin, user) {
        (Ok(return_type), None) => Ok(SelectedOverload::Builtin(return_type)),
        (Ok(_), Some(user)) if user.is_exact_for_known_arguments() => {
            Ok(SelectedOverload::User(user))
        }
        (Ok(_), Some(_)) => Err(ambiguous_function(name, args, argument_types)),
        (Err(_), Some(user)) => Ok(SelectedOverload::User(user)),
        (Err(error), None) => Err(error),
    }
}

fn resolve_builtin_type(
    name: &str,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> Result<ColumnType, SQLError> {
    resolve_builtin_type_with_control(
        name,
        args,
        argument_types,
        &ProductionControl::uncontrolled(),
    )
    .map(|ty| {
        ty.into_uncontrolled()
            .expect("ordinary array transform type")
    })
}

pub(super) fn resolve_type_with_control(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    let dispatched =
        binding.and_then(|binding| binding.dispatch) == Some(FunctionDispatch::ArraySortJson);
    let builtin = if !dispatched
        && explicit_variadic
        && args.iter().any(|arg| named_argument_name(arg).is_some())
    {
        Err(undefined_function(name, args, argument_types))
    } else {
        resolve_builtin_type_with_control(name, args, argument_types, control)
    };
    if !dispatched && binding.is_some() {
        return Err(builtin.err().unwrap_or_else(|| {
            function_resolution_error_borrowed(
                "42883",
                "does not exist",
                name,
                args,
                args.iter().zip(argument_types).map(|(argument, ty)| {
                    if matches!(
                        named_argument_value(argument),
                        ScalarExpr::Literal(Value::Str(_) | Value::Null)
                    ) {
                        None
                    } else {
                        ty.as_ref()
                    }
                }),
            )
        }));
    }
    builtin.map(Some)
}

fn resolve_builtin_type_with_control(
    name: &str,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    let names = argument_names_with_control(args, control)?;
    let Some(positions) =
        crate::expr::array_transform_argument_positions_with_control(name, &names, control)?
    else {
        return Err(undefined_function(name, args, argument_types));
    };
    // The selected signatures have at most three arguments; only borrowed type slots are reordered.
    let mut effective = [None; 3];
    let mut declared = [None; 3];
    for (index, ((argument, argument_type), position)) in args
        .iter()
        .zip(argument_types)
        .zip(positions.iter())
        .enumerate()
    {
        let argument = named_argument_value(argument);
        let ty = if matches!(argument, ScalarExpr::Literal(Value::Str(_) | Value::Null))
            || *position > 0 && matches!(argument, ScalarExpr::Param(_))
        {
            None
        } else {
            argument_type.as_ref()
        };
        effective[index] = ty;
        declared[*position] = ty;
    }
    if declared
        .iter()
        .skip(1)
        .flatten()
        .any(|ty| !matches!(base_type(ty), ColumnType::Boolean))
    {
        return Err(function_resolution_error_borrowed(
            "42883",
            "does not exist",
            name,
            args,
            effective.into_iter(),
        ));
    }
    match declared[0] {
        Some(ty) if is_array_type(ty) => Ok(base_type(ty).clone_with_control(control)?),
        None => Err(SQLError::Routine {
            sqlstate: "42804".into(),
            message: "could not determine polymorphic type because input has type unknown".into(),
        }),
        Some(_) => Err(function_resolution_error_borrowed(
            "42883",
            "does not exist",
            name,
            args,
            effective.into_iter(),
        )),
    }
}

fn argument_names_with_control<'a>(
    args: &'a [ScalarExpr],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Option<&'a str>>>, SQLError> {
    let mut names = ProductionVec::new(*control);
    names.reserve(args.len())?;
    for argument in args {
        names.push_copy(named_argument_name(argument))?;
    }
    Ok(names.finish()?)
}

fn resolve_user_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
    if binding.is_none() && name.to_ascii_lowercase().starts_with("pg_catalog.") {
        return Ok(None);
    }
    let Some(resolver) = resolver else {
        return Ok(None);
    };
    let argument_names = args
        .iter()
        .map(|argument| named_argument_name(argument).map(str::to_string))
        .collect::<Vec<_>>();
    resolver.resolve_function_overload(
        name,
        binding,
        &argument_names,
        &user_argument_types(args, argument_types),
        explicit_variadic,
    )
}

fn user_argument_types(
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> Vec<Option<ColumnType>> {
    args.iter()
        .zip(argument_types)
        .map(|(argument, argument_type)| {
            if matches!(
                named_argument_value(argument),
                ScalarExpr::Literal(Value::Str(_) | Value::Null)
            ) {
                None
            } else {
                argument_type.clone()
            }
        })
        .collect()
}

pub(super) fn is_function(name: &str) -> bool {
    let local = local_name(name);
    local.eq_ignore_ascii_case("array_sort") || local.eq_ignore_ascii_case("array_reverse")
}

pub(super) fn bind_call(
    name: String,
    binding: &mut Option<FunctionBinding>,
    args: &mut Vec<ScalarExpr>,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> String {
    if binding.is_some() || !is_function(&name) {
        return name;
    }
    let Ok(call_arguments) = scalar_call_arguments(args) else {
        return name;
    };
    let explicit_variadic = call_arguments
        .iter()
        .any(|argument| argument.explicit_variadic);
    let argument_types = call_arguments
        .iter()
        .map(|argument| super::scalar_type_inner(argument.value, schema, params, resolver))
        .collect::<Result<Vec<_>, _>>();
    if let Ok(SelectedOverload::User(resolved)) = argument_types.and_then(|argument_types| {
        select_overload(
            &name,
            None,
            args,
            &argument_types,
            explicit_variadic,
            resolver,
        )
    }) {
        *binding = Some(resolved.binding);
        return name;
    }
    let control = ProductionControl::uncontrolled();
    let mut infer = |expression: &ScalarExpr| {
        super::scalar_type_inner(expression, schema, params, resolver)
            .ok()
            .flatten()
            .map(|ty| {
                control
                    .finish(ty, control.empty_reservation())
                    .map_err(Into::into)
            })
            .transpose()
    };
    let call = control
        .finish(
            BindingCall {
                name,
                binding: binding.take(),
                arguments: std::mem::take(args),
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            },
            control.empty_reservation(),
        )
        .expect("ordinary binding owner");
    let call = bind_call_with_control(call, &mut infer, &control)
        .expect("ordinary array binding cannot be cancelled or limited")
        .into_uncontrolled()
        .expect("ordinary array binding");
    *binding = call.binding;
    *args = call.arguments;
    call.name
}

pub(super) fn bind_call_with_control(
    call: Produced<BindingCall>,
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<BindingCall>, SQLError> {
    let mut owner = super::call::CallOwner::new(call, control)?;
    bind_call_in_place_with_control(&mut owner.call, &mut owner.memory, infer, control)?;
    owner.finish(control)
}

/// The caller keeps the enclosing expression and its lease alive throughout mutation, including on errors and unwinding.
pub(super) fn bind_call_in_place_with_control(
    call: &mut BindingCall,
    memory: &mut Option<MemoryReservation>,
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    super::call::check_memory(memory.as_ref(), control)?;
    if call.binding.is_some() || !is_function(&call.name) {
        return Ok(());
    }
    for argument in &call.arguments {
        if crate::scalar_call_argument(argument).is_err() {
            return Ok(());
        }
    }
    let mut positions = {
        let names = argument_names_with_control(&call.arguments, control)?;
        match crate::expr::array_transform_argument_positions_with_control(
            &call.name, &names, control,
        ) {
            Ok(Some(positions)) => positions,
            Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
            Ok(None) | Err(_) => return Ok(()),
        }
    };
    let mut cast_names: [Option<Produced<String>>; 3] = [None, None, None];
    let mut selected = None;
    for (argument, position) in call.arguments.iter().zip(positions.iter().copied()) {
        let argument = named_argument_value(argument);
        let ty = if position > 0
            && matches!(
                argument,
                ScalarExpr::Param(_) | ScalarExpr::Literal(Value::Str(_) | Value::Null)
            ) {
            None
        } else {
            match super::call::infer_with_control(argument, infer, control) {
                Ok(ty) => ty,
                Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                    return Err(error)
                }
                Err(_) => None,
            }
        };
        if position > 0 && ty.is_none() {
            cast_names[position] = Some(control.copy_text("boolean")?);
        } else if position == 0
            && local_name(&call.name).eq_ignore_ascii_case("array_sort")
            && ty.as_deref().is_some_and(is_json_array_type)
        {
            selected = Some(FunctionBinding::dispatched_with_control(
                FunctionDispatch::ArraySortJson,
                control,
            )?);
        }
    }
    let extra = control.reserve(cast_names.iter().flatten().count() * size_of::<ScalarExpr>())?;
    *memory = control.combine(memory.take(), extra);
    for destination in 0..call.arguments.len() {
        let source = positions
            .iter()
            .position(|position| *position == destination)
            .expect("validated positions fill each slot");
        call.arguments.swap(destination, source);
        positions.as_mut_slice().swap(destination, source);
    }
    for (position, argument) in call.arguments.iter_mut().enumerate() {
        let expression = named_argument_value_owned(std::mem::replace(
            argument,
            ScalarExpr::Literal(Value::Null),
        ));
        *argument = if let Some(ty) = cast_names[position].take() {
            let (ty, extra) = ty.into_parts();
            *memory = control.combine(memory.take(), extra);
            ScalarExpr::Cast {
                expr: Box::new(expression),
                ty,
            }
        } else {
            expression
        };
    }
    if let Some(selected) = selected {
        let (selected, extra) = selected.into_parts();
        *memory = control.combine(memory.take(), extra);
        call.binding = Some(selected);
    }
    control.check()?;
    Ok(())
}

fn local_name(name: &str) -> &str {
    name.get(..11)
        .filter(|prefix| prefix.eq_ignore_ascii_case("pg_catalog."))
        .map_or(name, |_| &name[11..])
}

fn named_argument_name(expression: &ScalarExpr) -> Option<&str> {
    crate::scalar_call_argument(expression)
        .ok()
        .and_then(|argument| argument.name)
}

fn named_argument_value_owned(expression: ScalarExpr) -> ScalarExpr {
    if matches!(
        &expression,
        ScalarExpr::Func { binding, args, .. }
            if binding.as_ref().and_then(|binding| binding.dispatch)
                == Some(FunctionDispatch::NamedArgument)
                && args.len() == 2
    ) {
        let ScalarExpr::Func { mut args, .. } = expression else {
            unreachable!();
        };
        return args.pop().expect("named argument value follows its name");
    }
    expression
}

fn is_array_type(argument_type: &ColumnType) -> bool {
    matches!(
        base_type(argument_type),
        ColumnType::Array(_)
            | ColumnType::AnyArray
            | ColumnType::Int2Vector
            | ColumnType::OidVector
    )
}

fn is_json_array_type(argument_type: &ColumnType) -> bool {
    matches!(
        base_type(argument_type),
        ColumnType::Array(element) if matches!(base_type(element), ColumnType::Json)
    )
}

fn undefined_function(
    name: &str,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> SQLError {
    function_resolution_error("42883", "does not exist", name, args, argument_types)
}

fn ambiguous_function(
    name: &str,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> SQLError {
    let argument_types = user_argument_types(args, argument_types);
    function_resolution_error("42725", "is not unique", name, args, &argument_types)
}

fn function_resolution_error(
    sqlstate: &str,
    description: &str,
    name: &str,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> SQLError {
    function_resolution_error_borrowed(
        sqlstate,
        description,
        name,
        args,
        argument_types.iter().map(Option::as_ref),
    )
}

fn function_resolution_error_borrowed<'a>(
    sqlstate: &str,
    description: &str,
    name: &str,
    args: &[ScalarExpr],
    argument_types: impl Iterator<Item = Option<&'a ColumnType>>,
) -> SQLError {
    let signature = args
        .iter()
        .zip(argument_types)
        .map(|(argument, argument_type)| {
            let argument_type =
                argument_type.map_or_else(|| "unknown".into(), ColumnType::regtype_name);
            named_argument_name(argument).map_or(argument_type.clone(), |argument_name| {
                format!("{argument_name} => {argument_type}")
            })
        })
        .collect::<Vec<_>>()
        .join(", ");
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: format!("function {name}({signature}) {description}"),
    }
}
