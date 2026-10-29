//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Boolean flattening moves independent children through admitted traversal and result scratch.

use super::{
    literal_value_with_control, OptimizerConfig, OwnedScalarExpr, ProductionControl, SQLError,
    ScalarExpr, Value,
};
use crate::scratch::Buffer;
use uqa_sql::plan::OwnedScalarChildren;

fn literal(value: bool, storage: &ProductionControl<'_>) -> Result<OwnedScalarExpr, SQLError> {
    Ok(OwnedScalarExpr::literal(
        storage.finish(Value::Bool(value), storage.empty_reservation())?,
        storage,
    )?)
}

fn next(
    pending: &mut OwnedScalarChildren,
    parents: &mut Buffer<OwnedScalarChildren>,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedScalarExpr>, SQLError> {
    loop {
        control.check()?;
        if let Some(child) = pending.next() {
            return Ok(Some(child));
        }
        let Some(parent) = parents.pop() else {
            return Ok(None);
        };
        *pending = parent;
    }
}

pub(in crate::optimizer::scalar) fn optimize(
    expression: OwnedScalarExpr,
    and: bool,
    config: &OptimizerConfig,
    storage: &ProductionControl<'_>,
    mut rewrite: impl FnMut(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
) -> Result<OwnedScalarExpr, SQLError> {
    expression.rewrite(storage, |expression| {
        assert!(matches!(
            (&*expression, and),
            (ScalarExpr::And(_), true) | (ScalarExpr::Or(_), false)
        ));
        let mut pending = expression.into_children();
        let mut parents = Buffer::new(config.production);
        let mut kept = Buffer::new(config.production);
        let mut have_null = false;
        while let Some(item) = next(&mut pending, &mut parents, &config.production)? {
            if matches!(
                (&*item, and),
                (ScalarExpr::And(_), true) | (ScalarExpr::Or(_), false)
            ) {
                if pending.len() != 0 {
                    parents.push(pending)?;
                }
                pending = item.into_children();
                continue;
            }
            let item = item.rewrite(storage, &mut rewrite)?;
            config.production.check()?;
            match literal_value_with_control(&item, &config.production)?.as_deref() {
                Some(Value::Bool(value)) if *value != and => {
                    if config.enable_boolean_simplify {
                        let value = *value;
                        drop((item, pending, parents, kept));
                        return literal(value, storage);
                    }
                    kept.push(item)?;
                    // A decisive value stops rewriting even when simplification is disabled. Keep the already-open pending sequence, including untouched nested boolean nodes, without flattening or observing it.
                    while let Some(item) = next(&mut pending, &mut parents, &config.production)? {
                        kept.push(item)?;
                    }
                    break;
                }
                Some(Value::Bool(_)) if config.enable_boolean_simplify => {}
                Some(Value::Null) if config.enable_boolean_simplify => have_null = true,
                _ => kept.push(item)?,
            }
        }
        drop((pending, parents));
        if have_null {
            kept.push(OwnedScalarExpr::typed_null("boolean", storage)?)?;
        }
        if config.enable_boolean_simplify {
            match kept.len() {
                0 => {
                    drop(kept);
                    return literal(and, storage);
                }
                1 => {
                    let result = kept.pop().expect("one surviving boolean child");
                    drop(kept);
                    return Ok(result);
                }
                _ => {}
            }
        }
        Ok(OwnedScalarExpr::boolean(kept.into_iter(), and, storage)?)
    })
}

#[cfg(test)]
mod tests;
