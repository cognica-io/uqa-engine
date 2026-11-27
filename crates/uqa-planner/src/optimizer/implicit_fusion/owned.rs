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
    storage: &ProductionControl<'_>,
    workspace: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    Fusion {
        allow_unqualified_signals,
        storage: *storage,
        workspace: *workspace,
    }
    .rewrite(expression)
}

struct Fusion<'scope> {
    allow_unqualified_signals: bool,
    storage: ProductionControl<'scope>,
    workspace: ProductionControl<'scope>,
}

impl Fusion<'_> {
    fn rewrite(&self, expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        self.workspace.check()?;
        expression.rewrite(&self.storage, |expression| match &*expression {
            ScalarExpr::And(_) => self.conjunction(expression),
            ScalarExpr::Or(_) => {
                expression.map_children(&self.storage, |_, child| self.rewrite(child))
            }
            _ => Ok(expression),
        })
    }

    fn flatten(
        &self,
        expression: OwnedScalarExpr,
        output: &mut Buffer<OwnedScalarExpr>,
    ) -> Result<(), SQLError> {
        self.workspace.check()?;
        if matches!(&*expression, ScalarExpr::And(_)) {
            for child in expression.into_children() {
                self.flatten(child, output)?;
            }
        } else {
            output.push(self.rewrite(expression)?)?;
        }
        Ok(())
    }

    fn conjunction(&self, expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.workspace;
        let mut parts = Buffer::new(*control);
        self.flatten(expression, &mut parts)?;
        let mut signals = Signals::default();
        for part in parts.iter() {
            control.check()?;
            signals.observe(part);
        }
        if !signals.can_fuse(self.allow_unqualified_signals) {
            return Ok(OwnedScalarExpr::boolean_with_workspace(
                parts.into_iter(),
                true,
                &self.storage,
                control,
            )?);
        }
        let mut selected = Buffer::new(*control);
        let mut residual = Buffer::new(*control);
        for expression in parts {
            control.check()?;
            if classify_signal(&expression).is_some() {
                selected.push(calibrate(expression, &self.storage, control)?)?;
            } else {
                residual.push(expression)?;
            }
        }
        let fusion = OwnedScalarExpr::call_with_workspace(
            control.copy_text("fuse_bayesian_evidence")?,
            None,
            selected.into_iter(),
            &self.storage,
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
        Ok(OwnedScalarExpr::boolean_with_workspace(
            conjunction.into_iter(),
            true,
            &self.storage,
            control,
        )?)
    }
}

fn calibrate(
    expression: OwnedScalarExpr,
    storage: &ProductionControl<'_>,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let ScalarExpr::Func { name, .. } = &*expression else {
        unreachable!("selected retrieval call")
    };
    if !name.eq_ignore_ascii_case("text_match") {
        return Ok(expression);
    }
    Ok(expression.replace_call_name(control.copy_text("bayesian_match")?, storage)?)
}

#[cfg(test)]
mod tests;
