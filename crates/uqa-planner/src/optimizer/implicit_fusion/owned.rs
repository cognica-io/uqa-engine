//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fusion construction moves independently admitted leaves into one selected call and its residual predicate.

use super::{classify_signal, summary::Signals, ScalarExpr};
use crate::scratch::Buffer;
use uqa_core::memory::ProductionControl;
use uqa_sql::{plan::OwnedScalarExpr, SQLError};

pub(in crate::optimizer) fn rewrite(
    expression: OwnedScalarExpr,
    allow_unqualified_signals: bool,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    expression.rewrite(control, |expression| match &*expression {
        ScalarExpr::And(_) => conjunction(expression, allow_unqualified_signals, control),
        ScalarExpr::Or(_) => expression.map_children(control, |_, child| {
            rewrite(child, allow_unqualified_signals, control)
        }),
        _ => Ok(expression),
    })
}

fn flatten(
    expression: OwnedScalarExpr,
    output: &mut Buffer<OwnedScalarExpr>,
    allow_unqualified_signals: bool,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if matches!(&*expression, ScalarExpr::And(_)) {
        for child in expression.into_children() {
            flatten(child, output, allow_unqualified_signals, control)?;
        }
    } else {
        output.push(rewrite(expression, allow_unqualified_signals, control)?)?;
    }
    Ok(())
}

fn conjunction(
    expression: OwnedScalarExpr,
    allow_unqualified_signals: bool,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let mut parts = Buffer::new(*control);
    flatten(expression, &mut parts, allow_unqualified_signals, control)?;
    let mut signals = Signals::default();
    for part in parts.iter() {
        control.check()?;
        signals.observe(part);
    }
    if !signals.can_fuse(allow_unqualified_signals) {
        return Ok(OwnedScalarExpr::boolean(parts.into_iter(), true, control)?);
    }
    let mut selected = Buffer::new(*control);
    let mut residual = Buffer::new(*control);
    for expression in parts {
        control.check()?;
        if classify_signal(&expression).is_some() {
            selected.push(calibrate(expression, control)?)?;
        } else {
            residual.push(expression)?;
        }
    }
    let fusion = OwnedScalarExpr::call(
        control.copy_text("fuse_bayesian_evidence")?,
        None,
        selected.into_iter(),
        control,
    )?;
    if residual.is_empty() {
        drop(residual);
        return Ok(fusion);
    }
    let mut conjunction = Buffer::new(*control);
    conjunction.push(fusion)?;
    for expression in residual {
        control.check()?;
        conjunction.push(expression)?;
    }
    Ok(OwnedScalarExpr::boolean(
        conjunction.into_iter(),
        true,
        control,
    )?)
}

fn calibrate(
    expression: OwnedScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let ScalarExpr::Func { name, binding, .. } = &*expression else {
        unreachable!("selected retrieval call")
    };
    if !name.eq_ignore_ascii_case("text_match") {
        return Ok(expression);
    }
    let binding = binding
        .as_ref()
        .map(|binding| binding.clone_with_control(control))
        .transpose()?;
    Ok(expression.replace_call_metadata(control.copy_text("bayesian_match")?, binding, control)?)
}

#[cfg(test)]
mod tests;
