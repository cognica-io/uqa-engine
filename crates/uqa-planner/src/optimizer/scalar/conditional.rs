//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered simplification of conditional expressions.

use std::collections::VecDeque;

use uqa_core::memory::ProductionControl;
use uqa_sql::plan::OwnedScalarExpr;

mod owned;

use super::constants::literal_value_with_control;
use super::{optimize_scalar, OptimizerConfig, SQLError, ScalarExpr, Value};

pub(super) fn optimize_boolean(
    items: Vec<ScalarExpr>,
    and: bool,
    config: &OptimizerConfig,
) -> Result<ScalarExpr, SQLError> {
    let mut pending = VecDeque::from(items);
    let mut kept = Vec::new();
    let mut have_null = false;
    while let Some(item) = pending.pop_front() {
        match item {
            ScalarExpr::And(items) if and => {
                for item in items.into_iter().rev() {
                    pending.push_front(item);
                }
                continue;
            }
            ScalarExpr::Or(items) if !and => {
                for item in items.into_iter().rev() {
                    pending.push_front(item);
                }
                continue;
            }
            _ => {}
        }
        let item = optimize_scalar(item, config)?;
        match literal_value_with_control(&item, &config.production)?.as_deref() {
            Some(Value::Bool(value)) if *value != and => {
                if config.enable_boolean_simplify {
                    return Ok(ScalarExpr::Literal(Value::Bool(*value)));
                }
                kept.push(item);
                kept.extend(pending);
                break;
            }
            Some(Value::Bool(_)) if config.enable_boolean_simplify => {}
            Some(Value::Null) if config.enable_boolean_simplify => have_null = true,
            _ => kept.push(item),
        }
    }
    if have_null {
        kept.push(ScalarExpr::TypedLiteral {
            collation_oid: None,
            composite_source: None,
            value: Value::Null,
            ty: "boolean".into(),
            bound_type: None,
            parameter_index: None,
        });
    }
    if config.enable_boolean_simplify {
        match kept.len() {
            0 => return Ok(ScalarExpr::Literal(Value::Bool(and))),
            1 => return Ok(kept.remove(0)),
            _ => {}
        }
    }
    Ok(if and {
        ScalarExpr::And(kept)
    } else {
        ScalarExpr::Or(kept)
    })
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
