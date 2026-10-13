//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native pattern operators and escape helpers share `PostgreSQL` overload selection.

use crate::{ast::PatternOperator, ColumnType, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

use super::{fixed_builtin, operators};

pub(crate) struct Selection {
    pub(crate) identity: operators::BinaryOperatorCatalogEntry,
    operands: Produced<[ColumnType; 3]>,
}

impl Selection {
    pub(crate) fn argument(&self, index: usize) -> &ColumnType {
        &self.operands[index]
    }
}

pub(crate) fn select(
    operator: PatternOperator,
    arguments: &[Option<&ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Selection, SQLError> {
    control.check()?;
    if arguments.len() != 2 {
        return Err(SQLError::Internal(
            "invalid pattern operator operands".into(),
        ));
    }
    let operands = operators::named_binary_operator_types_with_control(
        operator.symbol(),
        arguments[0],
        arguments[1],
        control,
    )?;
    let identity = operators::named_binary_operator_catalog_entry(
        operator.symbol(),
        [&operands[0], &operands[1]],
    )?;
    Ok(Selection { identity, operands })
}

pub(crate) fn select_escape(
    similar: bool,
    arguments: &[Option<&ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    if !(arguments.len() == 2 || similar && arguments.len() == 1) {
        return Err(SQLError::Internal("invalid pattern escape operands".into()));
    }
    let name = if similar {
        "pg_catalog.similar_to_escape"
    } else {
        "pg_catalog.like_escape"
    };
    let mut inputs = ProductionVec::new(*control);
    inputs.reserve(arguments.len())?;
    for argument in arguments {
        match argument {
            Some(ty) => {
                let (ty, memory) = ty.clone_with_control(control)?.into_parts();
                inputs.push_produced(control.finish(Some(ty), memory)?)?;
            }
            None => inputs.push_produced(control.finish(None, control.empty_reservation())?)?,
        }
    }
    let inputs = inputs.finish()?;
    fixed_builtin::resolve_return_type_with_control(
        name,
        None,
        &[None, None][..arguments.len()],
        &inputs,
        false,
        control,
    )
}

#[cfg(test)]
mod tests;
