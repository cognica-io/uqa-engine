//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::{ColumnType, FunctionBinding};
use crate::{SQLError, SQLParam};
use uqa_core::memory::{Produced, ProductionControl};

use crate::{scalar_call_argument, schema::ScalarTypeSchema, ScalarExpr};

use super::{fixed_builtin, scalar_type_inner, FunctionTypeResolver};
mod production;
pub(super) use production::builtin_function_type_with_control;

#[derive(Clone, Copy)]
pub(super) struct FunctionTypeCall<'a> {
    pub name: &'a str,
    pub binding: Option<&'a FunctionBinding>,
    pub args: &'a [ScalarExpr],
}

pub fn builtin_function_type(
    name: &str,
    args: &[ScalarExpr],
    order_by: &[crate::ScalarOrder],
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
) -> Result<Option<ColumnType>, SQLError> {
    builtin_function_type_inner(name, None, args, order_by, schema, params, None)
}

/// Return the declared argument targets selected by PostgreSQL-compatible built-in resolution. Known argument types are retained for polymorphic calls, while fixed signatures and overloaded operators supply the context needed to resolve `unknown` arguments.
#[must_use]
pub fn builtin_function_argument_targets(
    name: &str,
    argument_types: &[Option<ColumnType>],
) -> Vec<Option<ColumnType>> {
    let lower = name.to_ascii_lowercase();
    let name = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
    if let Some(targets) = fixed_builtin::selected_argument_targets(name, argument_types) {
        return targets;
    }
    let mut targets = argument_types.to_vec();
    match name {
        "array_cat" | "array_append" | "array_prepend" | "array_remove" | "array_replace" => {
            if let Ok(Some(array)) = compatible_array_result_type(
                name,
                argument_types,
                &ProductionControl::uncontrolled(),
            ) {
                let ColumnType::Array(element) = &*array else {
                    unreachable!("compatible array result");
                };
                for (position, target) in targets.iter_mut().enumerate() {
                    *target = Some(if compatible_array_argument(name, position) {
                        (*array).clone()
                    } else {
                        (**element).clone()
                    });
                }
            }
        }
        "upper" | "lower" | "initcap" | "trim" | "btrim" | "ltrim" | "rtrim" | "analyze_text"
        | "create_analyzer" | "drop_analyzer" | "set_table_analyzer" | "fts_index_stats" => {
            targets.fill(Some(ColumnType::Text));
        }
        "array_sort" if matches!(targets.len(), 2 | 3) => {
            targets.iter_mut().skip(1).for_each(|target| {
                *target = Some(ColumnType::Boolean);
            });
        }
        "concat_op" if targets.len() == 2 => {
            for position in 0..2 {
                if targets[position].is_none() {
                    targets[position] = Some(concat_argument_type(targets[1 - position].as_ref()));
                }
            }
        }
        _ => {}
    }
    targets
}

fn compatible_array_argument(name: &str, position: usize) -> bool {
    name == "array_cat" || position == usize::from(name == "array_prepend")
}

pub(super) fn compatible_array_result_type(
    name: &str,
    argument_types: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let mut element: Option<Produced<ColumnType>> = None;
    for (position, argument) in argument_types.iter().enumerate() {
        control.check()?;
        let Some(argument) = argument else { continue };
        let candidate = if compatible_array_argument(name, position) {
            let Some(element) = super::array_element_type(argument) else {
                return Ok(None);
            };
            element
        } else {
            argument
        };
        element = Some(match element {
            None => candidate.clone_with_control(control)?,
            Some(previous) => {
                super::common::common_type_with_control(&previous, candidate, control)?
            }
        });
    }
    element
        .map(|element| ColumnType::array_with_control(element, control).map_err(Into::into))
        .transpose()
}

pub(super) fn builtin_function_type_inner(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    order_by: &[crate::ScalarOrder],
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
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
    builtin_function_type_with_control(
        FunctionTypeCall {
            name,
            binding,
            args,
        },
        order_by,
        params,
        resolver,
        &mut infer,
        &control,
    )
    .map(|ty| {
        ty.map(|ty| {
            ty.into_uncontrolled()
                .expect("ordinary builtin result type")
        })
    })
}

pub(super) fn named_argument(expression: &ScalarExpr) -> (Option<String>, &ScalarExpr) {
    scalar_call_argument(expression).map_or((None, expression), |argument| {
        (argument.name.map(str::to_string), argument.value)
    })
}

pub(super) fn named_argument_value(expression: &ScalarExpr) -> &ScalarExpr {
    scalar_call_argument(expression).map_or(expression, |argument| argument.value)
}

fn concat_argument_type(other: Option<&ColumnType>) -> ColumnType {
    match other {
        Some(array @ ColumnType::Array(_)) => array.clone(),
        Some(ColumnType::JsonB) => ColumnType::JsonB,
        _ => ColumnType::Text,
    }
}
