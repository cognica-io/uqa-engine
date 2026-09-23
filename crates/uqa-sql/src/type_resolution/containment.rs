//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` containment-operator type resolution and unknown-literal coercion.

use super::call::{BindingCall, InferType};
use crate::ast::ColumnType;
use crate::{SQLError, SQLParam};
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionControl},
    Value,
};

use crate::{schema::ScalarTypeSchema, ScalarExpr};

use super::common::base_type;
use super::functions::named_argument_value;
use super::{scalar_type_inner, FunctionTypeResolver};

pub(super) fn is_operator(name: &str) -> bool {
    matches!(name, "contains_op" | "contained_by_op")
}

pub(super) fn resolve_operator_type_with_control(
    name: &str,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    if args.len() != 2 {
        return Err(SQLError::BadArity {
            name: name.into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let left = argument_type(&args[0], argument_types[0].as_ref());
    let right = argument_type(&args[1], argument_types[1].as_ref());
    let symbol = operator_symbol(name);
    let compatible = match (&left, &right) {
        (Some(left), Some(right)) => match (base_type(left), base_type(right)) {
            (ColumnType::JsonB, ColumnType::JsonB) => true,
            (left @ ColumnType::Array(_), right @ ColumnType::Array(_)) => {
                super::common::same_operator_type_with_control(left, right, control)?
            }
            (
                ColumnType::Range(left) | ColumnType::Multirange(left),
                ColumnType::Range(right) | ColumnType::Multirange(right),
            ) => left == right,
            _ => false,
        },
        (Some(known), None) | (None, Some(known)) => supported_type(known),
        (None, None) => {
            return Err(SQLError::Routine {
                sqlstate: "42725".into(),
                message: format!("operator is not unique: unknown {symbol} unknown"),
            });
        }
    };
    if compatible {
        return Ok(Some(
            control.finish(ColumnType::Boolean, control.empty_reservation())?,
        ));
    }
    Err(SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!(
            "operator does not exist: {} {symbol} {}",
            type_name(left),
            type_name(right)
        ),
    })
}

pub(super) fn bind_unknown_arguments(
    args: &mut [ScalarExpr],
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) {
    let control = ProductionControl::uncontrolled();
    let mut infer = |expression: &ScalarExpr| {
        scalar_type_inner(expression, schema, params, resolver)?
            .map(|ty| {
                control
                    .finish(ty, control.empty_reservation())
                    .map_err(Into::into)
            })
            .transpose()
    };
    if let Ok(Some(prepared)) = prepare_cast(args, &mut infer, &control) {
        let (index, ty) = prepared
            .into_uncontrolled()
            .expect("ordinary containment cast");
        install_cast(args, index, ty);
    }
}

#[cfg(test)]
pub(super) fn bind_unknown_arguments_with_control(
    call: Produced<BindingCall>,
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<BindingCall>, SQLError> {
    let mut owner = super::call::CallOwner::new(call, control)?;
    bind_unknown_arguments_in_place_with_control(
        &mut owner.call,
        &mut owner.memory,
        infer,
        control,
    )?;
    owner.finish(control)
}

/// The caller keeps the enclosing expression and its lease alive throughout mutation, including on errors and unwinding.
pub(super) fn bind_unknown_arguments_in_place_with_control(
    call: &mut BindingCall,
    memory: &mut Option<MemoryReservation>,
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    super::call::check_memory(memory.as_ref(), control)?;
    let Some(prepared) = prepare_cast(&call.arguments, infer, control)? else {
        return Ok(());
    };
    let ((index, ty), extra) = prepared.into_parts();
    *memory = control.combine(memory.take(), extra);
    install_cast(&mut call.arguments, index, ty);
    control.check()?;
    Ok(())
}

fn install_cast(args: &mut [ScalarExpr], index: usize, ty: String) {
    let inner = std::mem::replace(&mut args[index], ScalarExpr::Literal(Value::Null));
    args[index] = ScalarExpr::Cast {
        expr: Box::new(inner),
        ty,
    };
}

fn prepare_cast(
    args: &[ScalarExpr],
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<(usize, String)>>, SQLError> {
    control.check()?;
    if args.len() != 2 {
        return Ok(None);
    }
    let unknown = [is_unknown_literal(&args[0]), is_unknown_literal(&args[1])];
    let index = match unknown {
        [true, false] => 0,
        [false, true] => 1,
        _ => return Ok(None),
    };
    let known_type = match super::call::infer_with_control(
        named_argument_value(&args[1 - index]),
        infer,
        control,
    ) {
        Ok(Some(ty)) => ty,
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
        Ok(None) | Err(_) => return Ok(None),
    };
    let target = base_type(&known_type);
    if !supported_type(target) {
        return Ok(None);
    }
    let ty = target.sql_name_with_control(control)?;
    let extra = control.reserve(size_of::<ScalarExpr>())?;
    let (ty, memory) = ty.into_parts();
    Ok(Some(
        control.finish((index, ty), control.combine(memory, extra))?,
    ))
}

fn argument_type<'a>(
    expression: &ScalarExpr,
    resolved_type: Option<&'a ColumnType>,
) -> Option<&'a ColumnType> {
    if is_unknown_literal(expression) {
        None
    } else {
        resolved_type
    }
}

fn is_unknown_literal(expression: &ScalarExpr) -> bool {
    matches!(
        named_argument_value(expression),
        ScalarExpr::Literal(Value::Str(_) | Value::Null)
    )
}

fn supported_type(ty: &ColumnType) -> bool {
    matches!(
        base_type(ty),
        ColumnType::Array(_) | ColumnType::JsonB | ColumnType::Range(_) | ColumnType::Multirange(_)
    )
}

fn type_name(ty: Option<&ColumnType>) -> String {
    ty.map_or_else(|| "unknown".into(), ColumnType::sql_name)
}

fn operator_symbol(name: &str) -> &'static str {
    if name == "contains_op" {
        "@>"
    } else {
        "<@"
    }
}
