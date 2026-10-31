//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible binding for `array_sort` and `array_reverse`.

mod owned;
mod preparation;
mod production;
pub(super) use owned::bind_owned_call;
pub(super) use production::resolve_type_with_resolver_and_control;

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

pub(crate) struct ResolvedArrayTransformCall {
    pub(crate) overload: Option<ResolvedFunctionOverload>,
}

/// Select the same candidate during preparation and expression binding, before
/// unknown arguments run their selected input conversions.
pub(crate) fn resolve_array_transform_call(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    params: &[SQLParam],
    explicit_variadic: bool,
    resolver: &dyn FunctionTypeResolver,
) -> Result<Option<ResolvedArrayTransformCall>, SQLError> {
    Ok(resolve_array_transform_call_with_control(
        name,
        binding,
        ArrayTransformArguments {
            expressions: args,
            types: argument_types,
            parameters: params,
            explicit_variadic,
        },
        resolver,
        &ProductionControl::uncontrolled(),
    )?
    .map(|call| ResolvedArrayTransformCall {
        overload: call.overload.map(|selected| {
            selected
                .into_uncontrolled()
                .expect("ordinary array selection")
        }),
    }))
}

pub(crate) struct ControlledArrayTransformCall {
    pub(crate) overload: Option<Produced<ResolvedFunctionOverload>>,
    pub(crate) builtin_argument_positions: Option<Produced<Vec<usize>>>,
}

pub(crate) struct ArrayTransformArguments<'a> {
    pub expressions: &'a [ScalarExpr],
    pub types: &'a [Option<ColumnType>],
    pub parameters: &'a [SQLParam],
    pub explicit_variadic: bool,
}

pub(crate) fn resolve_array_transform_call_with_control(
    name: &str,
    binding: Option<&FunctionBinding>,
    arguments: ArrayTransformArguments<'_>,
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<Option<ControlledArrayTransformCall>, SQLError> {
    let ArrayTransformArguments {
        expressions: args,
        types: argument_types,
        parameters: params,
        explicit_variadic,
    } = arguments;
    control.check()?;
    if !is_function(name) {
        return Ok(None);
    }
    let effective = argument_types_with_parameters(name, args, argument_types, params, control)?;
    let (overload, positions) = match production::select_overload_with_control(
        name,
        binding,
        args,
        &effective,
        explicit_variadic,
        Some(resolver),
        control,
    )? {
        production::OwnedSelection::User(selected) => (Some(selected), None),
        production::OwnedSelection::Builtin(_) => {
            let names = argument_names_with_control(args, control)?;
            (
                None,
                crate::expr::array_transform_argument_positions_with_control(
                    name, &names, control,
                )?,
            )
        }
    };
    Ok(Some(ControlledArrayTransformCall {
        overload,
        builtin_argument_positions: positions,
    }))
}

fn select_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
    production::select_overload_with_control(
        name,
        binding,
        args,
        argument_types,
        explicit_variadic,
        resolver,
        &ProductionControl::uncontrolled(),
    )
    .map(|selected| match selected {
        production::OwnedSelection::Builtin(_) => None,
        production::OwnedSelection::User(selected) => Some(
            selected
                .into_uncontrolled()
                .expect("ordinary array overload"),
        ),
    })
}

#[cfg(test)]
pub(super) fn resolve_type_with_control(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    resolve_type_with_resolver_and_control(
        name,
        binding,
        args,
        argument_types,
        explicit_variadic,
        None,
        control,
    )
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
        let ty = if matches!(argument, ScalarExpr::Literal(Value::Str(_) | Value::Null)) {
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

/// Unknown string and NULL parameters take Boolean context in option slots.
/// Other scalar values and explicitly declared parameter types stay authoritative.
pub(super) fn argument_types_with_parameters(
    name: &str,
    args: &[ScalarExpr],
    types: &[Option<ColumnType>],
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Option<ColumnType>>>, SQLError> {
    let names = argument_names_with_control(args, control)?;
    let positions =
        crate::expr::array_transform_argument_positions_with_control(name, &names, control)?;
    let mut effective = ProductionVec::new(*control);
    effective.reserve(types.len())?;
    for (index, (argument, ty)) in args.iter().zip(types).enumerate() {
        let untyped_option = positions
            .as_ref()
            .is_some_and(|positions| positions[index] > 0)
            && matches!(named_argument_value(argument), ScalarExpr::Param(index)
                if index.checked_sub(1).and_then(|index| params.get(index))
                    .is_some_and(|parameter| matches!(parameter, SQLParam::Scalar(Value::Str(_) | Value::Null))));
        let ty = if untyped_option { None } else { ty.as_ref() };
        effective.push_produced(match ty {
            Some(ty) => {
                let (ty, memory) = ty.clone_with_control(control)?.into_parts();
                control.finish(Some(ty), memory)?
            }
            None => control.finish(None, control.empty_reservation())?,
        })?;
    }
    Ok(effective.finish()?)
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

pub(crate) fn is_function(name: &str) -> bool {
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
    if let Ok(Some(resolved)) = argument_types.and_then(|argument_types| {
        let argument_types = argument_types_with_parameters(
            &name,
            args,
            &argument_types,
            params,
            &ProductionControl::uncontrolled(),
        )?;
        select_overload(
            &name,
            None,
            args,
            &argument_types,
            explicit_variadic,
            resolver,
        )
    }) {
        FunctionBinding::rebind(binding, resolved.binding);
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
                order_syntax: crate::ast::FunctionOrderSyntax::Ordinary,
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
    let Some(preparation::Prepared {
        mut positions,
        mut cast_names,
        selected,
    }) = preparation::prepare(&call.name, &call.arguments, infer, control)?
    else {
        return Ok(());
    };
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
                implicit: true,
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
        FunctionBinding::rebind(&mut call.binding, selected);
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
    SQLError::function_call_resolution(sqlstate, &format!("{name}({signature})"), description)
}
