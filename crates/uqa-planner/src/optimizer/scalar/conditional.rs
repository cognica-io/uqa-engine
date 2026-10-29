//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered simplification of conditional expressions.

use uqa_core::memory::ProductionControl;
use uqa_sql::plan::OwnedScalarExpr;

mod boolean;
mod owned;

use super::constants::literal_value_with_control;
use super::{optimize_scalar, OptimizerConfig, SQLError, ScalarExpr, Value};

pub(super) fn optimize_boolean(
    items: Vec<ScalarExpr>,
    and: bool,
    config: &OptimizerConfig,
) -> Result<ScalarExpr, SQLError> {
    let storage = ProductionControl::uncontrolled();
    let expression = if and {
        ScalarExpr::And(items)
    } else {
        ScalarExpr::Or(items)
    };
    let output = boolean::optimize(
        OwnedScalarExpr::uncontrolled(expression),
        and,
        config,
        &storage,
        |child| optimize_ordinary_child(child, config),
    )?;
    Ok(output.finish(&storage)?.into_parts().0)
}

pub(super) fn optimize_case(
    expression: ScalarExpr,
    config: &OptimizerConfig,
) -> Result<ScalarExpr, SQLError> {
    let storage = ProductionControl::uncontrolled();
    let output = owned::optimize_case(
        OwnedScalarExpr::uncontrolled(expression),
        config,
        &storage,
        |child| optimize_ordinary_child(child, config),
    )?;
    Ok(output.finish(&storage)?.into_parts().0)
}

pub(super) fn optimize_coalesce(
    expression: ScalarExpr,
    config: &OptimizerConfig,
) -> Result<ScalarExpr, SQLError> {
    let storage = ProductionControl::uncontrolled();
    let output = owned::optimize_coalesce(
        OwnedScalarExpr::uncontrolled(expression),
        config,
        &storage,
        |child| optimize_ordinary_child(child, config),
    )?;
    Ok(output.finish(&storage)?.into_parts().0)
}

// Existing native mutable plans enter as ordinary storage. New constant payloads keep their separate production owners; moving the complete scalar pipeline to admitted input remains a distinct boundary.
fn optimize_ordinary_child(
    child: OwnedScalarExpr,
    config: &OptimizerConfig,
) -> Result<OwnedScalarExpr, SQLError> {
    let (expression, _) = child
        .finish(&ProductionControl::uncontrolled())?
        .into_parts();
    Ok(OwnedScalarExpr::uncontrolled(optimize_scalar(
        expression, config,
    )?))
}
