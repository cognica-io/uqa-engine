//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` accepts polymorphic cast targets before it tries ordinary binary or I/O coercion.

use crate::{ast::ExpressionPseudoType as P, ColumnType, SQLError};
use uqa_core::memory::ProductionControl;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Coercion<'a> {
    Preserve,
    Unknown,
    Relabel(&'a ColumnType),
}

/// Classify an already resolved catalog target and operand. An absent source denotes an unknown input. Only a non-null unknown constant invokes the pseudo-type's rejecting input function during analysis.
pub(crate) fn select<'a>(
    target: &'a ColumnType,
    source: Option<&'a ColumnType>,
    unknown_string: bool,
    control: &ProductionControl<'_>,
) -> Result<Option<Coercion<'a>>, SQLError> {
    if !matches!(target, ColumnType::Pseudo(_) | ColumnType::AnyArray) {
        return Ok(None);
    }
    control.check()?;
    let source = source.filter(|ty| !matches!(ty, ColumnType::Pseudo(P::Unknown)));
    if matches!(
        target,
        ColumnType::Pseudo(P::Any | P::AnyElement | P::AnyCompatible)
    ) {
        return Ok(Some(Coercion::Preserve));
    }
    let base = source
        .map(|ty| {
            crate::catalog::work::inline(super::common::base_type_with_work(ty, control, &mut ()))
        })
        .transpose()?;
    let array = matches!(
        base,
        Some(ColumnType::Array(_) | ColumnType::Int2Vector | ColumnType::OidVector)
    );
    let coercion = match target {
        ColumnType::Pseudo(P::AnyNonArray | P::AnyCompatibleNonArray) => {
            (!array).then_some(Coercion::Preserve)
        }
        ColumnType::Pseudo(P::Unknown) => {
            if source.is_none() {
                Some(Coercion::Preserve)
            } else {
                base.filter(|ty| super::cast_compatibility::is_string_io_type(ty))
                    .map(|_| Coercion::Unknown)
            }
        }
        ColumnType::Pseudo(P::AnyEnum) => source
            .filter(|ty| matches!(ty, ColumnType::Enum(_)))
            .map(|_| Coercion::Preserve),
        ColumnType::AnyArray
        | ColumnType::Pseudo(
            P::AnyCompatibleArray
            | P::AnyRange
            | P::AnyCompatibleRange
            | P::AnyMultiRange
            | P::AnyCompatibleMultiRange,
        ) => {
            if let Some(base) = base {
                let matches = source == Some(target)
                    || match target {
                        ColumnType::AnyArray | ColumnType::Pseudo(P::AnyCompatibleArray) => array,
                        ColumnType::Pseudo(P::AnyRange | P::AnyCompatibleRange) => {
                            matches!(base, ColumnType::Range(_) | ColumnType::DeclaredRange(_))
                        }
                        _ => matches!(
                            base,
                            ColumnType::Multirange(_) | ColumnType::DeclaredMultirange(_)
                        ),
                    };
                matches.then_some(if matches!(source, Some(ColumnType::Domain { .. })) {
                    Coercion::Relabel(base)
                } else {
                    Coercion::Preserve
                })
            } else if unknown_string {
                let name = target.regtype_name_with_control(control)?;
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: control
                        .format(format_args!("cannot accept a value of type {}", &**name))?
                        .into_parts()
                        .0,
                });
            } else {
                Some(Coercion::Relabel(target))
            }
        }
        _ => return Ok(None),
    };
    if coercion.is_some() {
        return Ok(coercion);
    }
    let name = source
        .map(|ty| ty.regtype_name_with_control(control))
        .transpose()?;
    let target = target.regtype_name_with_control(control)?;
    Err(SQLError::Routine {
        sqlstate: "42846".into(),
        message: control
            .format(format_args!(
                "cannot cast type {} to {}",
                name.as_deref().map_or("unknown", String::as_str),
                &**target
            ))?
            .into_parts()
            .0,
    })
}

#[cfg(test)]
mod tests;
