//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow selected concatenation inputs; only compatible-array selection owns a result type.

use super::{
    common::base_type,
    functions::{array_concat_argument, array_concat_types},
    operators::{binary_operator_by_oid, BinaryOperatorCatalogEntry},
};
use crate::{
    ast::{FunctionBinding, FunctionDispatch},
    ColumnType, FunctionTypeResolver, SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

/// Previous native concatenations have no structural identity; registered callbacks remain ordinary calls.
pub(crate) fn requires_binding(
    name: &str,
    binding: Option<&FunctionBinding>,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> bool {
    matches!(name, "concat_op" | "pg_catalog.concat_op")
        && !(binding.is_none()
            && resolver.is_some_and(|resolver| resolver.has_untyped_function(name)))
        && binding.is_none_or(|binding| {
            binding.builtin && binding.dispatch.is_none() && binding.resolution_error.is_none()
        })
}

pub(crate) enum Concatenation<'a> {
    Array {
        dispatch: FunctionDispatch,
        ty: Produced<ColumnType>,
    },
    Scalar {
        oid: i64,
        inputs: [&'a ColumnType; 2],
        result: &'static ColumnType,
    },
}

impl Concatenation<'_> {
    pub(crate) fn binding(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<FunctionBinding>, SQLError> {
        let (mut binding, mut memory) =
            FunctionBinding::dispatched_with_control(self.dispatch(), control)?.into_parts();
        if matches!(
            binding.dispatch,
            Some(FunctionDispatch::Concat {
                oid: Some(2779 | 2780)
            })
        ) {
            let mut names = uqa_core::memory::ProductionVec::new(*control);
            names.reserve(2)?;
            for position in 0..2 {
                names.push_produced(self.argument(position).catalog_name_with_control(control)?)?;
            }
            let (arguments, arguments_memory) = names.finish()?.into_parts();
            binding.argument_types = arguments;
            memory = control.combine(memory, arguments_memory);
        }
        Ok(control.finish(binding, memory)?)
    }

    pub(crate) fn dispatch(&self) -> FunctionDispatch {
        match self {
            Self::Array { dispatch, .. } => *dispatch,
            Self::Scalar { oid, .. } => FunctionDispatch::Concat {
                oid: Some(u32::try_from(*oid).expect("catalog operator identity")),
            },
        }
    }

    pub(crate) fn argument(&self, position: usize) -> &ColumnType {
        match self {
            Self::Array { dispatch, ty } => {
                if array_concat_argument(*dispatch, position) {
                    ty
                } else {
                    let ColumnType::Array(element) = &**ty else {
                        unreachable!("selected array concatenation type")
                    };
                    element
                }
            }
            Self::Scalar { inputs, .. } => inputs[position],
        }
    }

    pub(crate) fn operator(&self) -> BinaryOperatorCatalogEntry {
        let oid = match self {
            Self::Array { dispatch, .. } => match dispatch {
                FunctionDispatch::ArrayConcat => 375,
                FunctionDispatch::ArrayAppend => 349,
                FunctionDispatch::ArrayPrepend => 374,
                _ => unreachable!("selected array concatenation dispatch"),
            },
            Self::Scalar { oid, .. } => *oid,
        };
        binary_operator_by_oid(oid).expect("captured concatenation catalog identity")
    }

    pub(crate) fn into_result(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ColumnType>, SQLError> {
        match self {
            Self::Array { ty, .. } => Ok(ty),
            Self::Scalar { result, .. } => Ok(result.clone_with_control(control)?),
        }
    }
}

pub(crate) fn select<'a>(
    left: Option<&'a ColumnType>,
    right: Option<&'a ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Concatenation<'a>, SQLError> {
    control.check()?;
    if let Some((dispatch, ty)) = array_concat_types(left, right, control)? {
        return Ok(Concatenation::Array { dispatch, ty });
    }
    let original = [left, right];
    let inputs = [left.map(base_type), right.map(base_type)];
    let (oid, inputs, result) = match inputs {
        [Some(ColumnType::JsonB), Some(ColumnType::JsonB) | None]
        | [None, Some(ColumnType::JsonB)] => (
            3284,
            [&ColumnType::JsonB, &ColumnType::JsonB],
            &ColumnType::JsonB,
        ),
        [Some(ColumnType::Bytea), Some(ColumnType::Bytea) | None]
        | [None, Some(ColumnType::Bytea)] => (
            2018,
            [&ColumnType::Bytea, &ColumnType::Bytea],
            &ColumnType::Bytea,
        ),
        [left, right] => match (
            left.is_none_or(text_coercible),
            right.is_none_or(text_coercible),
        ) {
            (true, true) => (
                654,
                [&ColumnType::Text, &ColumnType::Text],
                &ColumnType::Text,
            ),
            (true, false) => (
                2779,
                [
                    &ColumnType::Text,
                    original[1].expect("known non-text operand"),
                ],
                &ColumnType::Text,
            ),
            (false, true) => (
                2780,
                [
                    original[0].expect("known non-text operand"),
                    &ColumnType::Text,
                ],
                &ColumnType::Text,
            ),
            (false, false) => {
                return Err(super::undefined_binary_operator(
                    original[0],
                    "||",
                    original[1],
                ))
            }
        },
    };
    Ok(Concatenation::Scalar {
        oid,
        inputs,
        result,
    })
}

fn text_coercible(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::Text
            | ColumnType::Varchar(_)
            | ColumnType::Character(_)
            | ColumnType::Bpchar
            | ColumnType::Name
            | ColumnType::InternalChar
    )
}
