//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible overload resolution for `random(min, max)`.

use super::common::base_type;
use super::functions::named_argument_value;
use super::{scalar_type_inner, FunctionTypeResolver};
use crate::{RowSchema, ScalarExpr};
use uqa_core::Value;
use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::expr::{RANDOM_INT4_FUNCTION, RANDOM_INT8_FUNCTION, RANDOM_NUMERIC_FUNCTION};
use uqa_sql::{SQLError, SQLParam};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RandomRangeOverload {
    Int4,
    Int8,
    Numeric,
}

pub(super) fn resolve_type(
    name: &str,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> Result<Option<ColumnType>, SQLError> {
    let argument_types = effective_argument_types(args, argument_types);
    validate_argument_order(args)?;
    if args.len() != 2 || !valid_argument_positions(args) {
        return Err(undefined_function(name, args, &argument_types));
    }
    match select_overload(&argument_types) {
        Some(overload) => Ok(Some(overload.return_type())),
        None if ambiguous_arguments(&argument_types) => {
            Err(ambiguous_function(name, args, &argument_types))
        }
        None => Err(undefined_function(name, args, &argument_types)),
    }
}

pub(super) fn selected_argument_type(argument_types: &[Option<ColumnType>]) -> Option<ColumnType> {
    select_overload(argument_types).map(RandomRangeOverload::return_type)
}

pub(super) fn bound_function_type(name: &str) -> Option<ColumnType> {
    Some(match name {
        RANDOM_INT4_FUNCTION => ColumnType::Integer,
        RANDOM_INT8_FUNCTION => ColumnType::BigInteger,
        RANDOM_NUMERIC_FUNCTION => numeric_type(),
        _ => return None,
    })
}

pub(super) fn is_function(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.strip_prefix("pg_catalog.").unwrap_or(&lower) == "random"
}

pub(super) fn bind_overload(
    name: String,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> String {
    if binding.is_some()
        || !is_function(&name)
        || args.len() != 2
        || !valid_argument_positions(args)
    {
        return name;
    }
    let argument_types = args
        .iter()
        .map(|argument| {
            let value = named_argument_value(argument);
            if matches!(value, ScalarExpr::Literal(Value::Str(_) | Value::Null)) {
                return None;
            }
            scalar_type_inner(value, schema, params, resolver)
                .ok()
                .flatten()
        })
        .collect::<Vec<_>>();
    match select_overload(&argument_types) {
        Some(RandomRangeOverload::Int4) => RANDOM_INT4_FUNCTION.into(),
        Some(RandomRangeOverload::Int8) => RANDOM_INT8_FUNCTION.into(),
        Some(RandomRangeOverload::Numeric) => RANDOM_NUMERIC_FUNCTION.into(),
        None => name,
    }
}

fn select_overload(argument_types: &[Option<ColumnType>]) -> Option<RandomRangeOverload> {
    if argument_types.len() != 2 {
        return None;
    }
    let mut strongest = None;
    for argument_type in argument_types.iter().flatten() {
        let rank = match base_type(argument_type) {
            ColumnType::SmallInteger => 0,
            ColumnType::Integer => 1,
            ColumnType::BigInteger => 2,
            ColumnType::Numeric { .. } => 3,
            _ => return None,
        };
        strongest = Some(strongest.map_or(rank, |current: u8| current.max(rank)));
    }
    Some(match strongest? {
        1 => RandomRangeOverload::Int4,
        2 => RandomRangeOverload::Int8,
        3 => RandomRangeOverload::Numeric,
        _ => return None,
    })
}

fn effective_argument_types(
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

fn ambiguous_arguments(argument_types: &[Option<ColumnType>]) -> bool {
    argument_types.len() == 2
        && argument_types.iter().all(|argument_type| {
            argument_type.as_ref().is_none_or(|argument_type| {
                matches!(base_type(argument_type), ColumnType::SmallInteger)
            })
        })
}

fn valid_argument_positions(args: &[ScalarExpr]) -> bool {
    if validate_argument_order(args).is_err() {
        return false;
    }
    let mut positions = [false; 2];
    let mut positional = 0;
    for argument in args {
        let position = match named_argument_name(argument) {
            Some("min") => 0,
            Some("max") => 1,
            Some(_) => return false,
            None => {
                let position = positional;
                positional += 1;
                position
            }
        };
        let Some(occupied) = positions.get_mut(position) else {
            return false;
        };
        if *occupied {
            return false;
        }
        *occupied = true;
    }
    positions.into_iter().all(|occupied| occupied)
}

fn validate_argument_order(args: &[ScalarExpr]) -> Result<(), SQLError> {
    uqa_sql::expr::validate_named_argument_order(args.iter().map(named_argument_name))
}

fn named_argument_name(expression: &ScalarExpr) -> Option<&str> {
    let ScalarExpr::Func { name, args, .. } = expression else {
        return None;
    };
    if name != uqa_sql::expr::NAMED_ARG_FUNCTION {
        return None;
    }
    match args.first() {
        Some(ScalarExpr::Literal(Value::Str(name))) => Some(name),
        _ => None,
    }
}

fn ambiguous_function(
    name: &str,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> SQLError {
    SQLError::Routine {
        sqlstate: "42725".into(),
        message: format!(
            "function {name}({}) is not unique",
            signature(args, argument_types)
        ),
    }
}

fn undefined_function(
    name: &str,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> SQLError {
    SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!(
            "function {name}({}) does not exist",
            signature(args, argument_types)
        ),
    }
}

fn signature(args: &[ScalarExpr], argument_types: &[Option<ColumnType>]) -> String {
    args.iter()
        .zip(argument_types)
        .map(|(argument, argument_type)| {
            let argument_type = argument_type
                .as_ref()
                .map_or_else(|| "unknown".into(), ColumnType::regtype_name);
            named_argument_name(argument).map_or(argument_type.clone(), |name| {
                format!("{name} => {argument_type}")
            })
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn numeric_type() -> ColumnType {
    ColumnType::Numeric {
        precision: None,
        scale: None,
    }
}

impl RandomRangeOverload {
    fn return_type(self) -> ColumnType {
        match self {
            Self::Int4 => ColumnType::Integer,
            Self::Int8 => ColumnType::BigInteger,
            Self::Numeric => numeric_type(),
        }
    }
}
