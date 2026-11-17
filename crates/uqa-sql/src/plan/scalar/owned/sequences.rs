//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored sequence edits retain the selected name's original owner and preserve surrounding call and cast metadata.

use super::{traversal::Edit, OwnedScalarExpr, ScalarExpr};
use crate::{
    binding::view_dependencies::{is_regclass_cast, is_sequence_function},
    plan::scalar::resources::input::Work,
    SQLError,
};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Produced, ProductionControl},
    Value,
};

struct Bind<F>(F);

impl<F, W: Work> Edit<W> for Bind<F>
where
    F: FnMut(&str, &ProductionControl<'_>) -> Result<Produced<String>, SQLError> + Send,
    W::Error: Into<SQLError>,
{
    type Identity = ();
    const BEFORE_CHILDREN: bool = false;
    fn identity(_: &ScalarExpr) {}

    async fn apply(
        &mut self,
        (): (),
        mut expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<OwnedScalarExpr, SQLError> {
        if !matches!(&*expression, ScalarExpr::Func { name, args, .. }
            if !args.is_empty() && is_sequence_function(name))
        {
            return Ok(expression);
        }
        let ScalarExpr::Func { args, .. } = &mut expression.expression else {
            unreachable!()
        };
        let mut value = &mut args[0];
        let mut memory = expression
            .memory
            .as_mut()
            .map(|owner| owner.children[0].as_mut().expect("sequence argument owner"));
        loop {
            control.check()?;
            work.checkpoint().await.map_err(Into::into)?;
            control.check()?;
            match value {
                ScalarExpr::Literal(Value::Str(reference)) => {
                    let (name, reservation) = (self.0)(reference, control)?.into_parts();
                    let replacement = OwnedScalarExpr::literal(
                        control.finish(Value::Str(name), reservation)?,
                        control,
                    )?;
                    let (replacement, owner) = replacement.into_rewrite_parts();
                    let old = std::mem::replace(value, replacement);
                    let old_owner = memory.map(|memory| {
                        std::mem::replace(memory, owner.expect("controlled sequence name"))
                    });
                    drop(OwnedScalarExpr::from_rewrite_parts(old, old_owner));
                    break;
                }
                ScalarExpr::Cast { expr, ty, .. } if is_regclass_cast(ty) => {
                    value = expr;
                    memory = memory
                        .map(|owner| owner.children[0].as_mut().expect("regclass operand owner"));
                }
                _ => break,
            }
        }
        control.check()?;
        Ok(expression)
    }
}

impl OwnedScalarExpr {
    /// Bind sequence string arguments in the existing children-before-parent order. Only builtin sequence names and regclass casts select a reference; the resolver returns an already admitted canonical name.
    pub fn bind_sequence_references_with_control<F>(
        self,
        resolve: &mut F,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError>
    where
        F: FnMut(&str, &ProductionControl<'_>) -> Result<Produced<String>, SQLError> + Send,
    {
        crate::catalog::work::inline(self.edit_from((), control, &mut (), &mut Bind(resolve)))
    }

    pub(in crate::plan) async fn bind_sequence_references_from<W: Work, F>(
        self,
        resolve: &mut F,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Self, SQLError>
    where
        W::Error: Into<SQLError>,
        F: FnMut(&str, &ProductionControl<'_>) -> Result<Produced<String>, SQLError> + Send,
    {
        self.edit_from((), control, work, &mut Bind(resolve)).await
    }

    /// Resume the same scalar binding under the original destination allowance. Query traversal must finish relation binding before invoking this sequence pass. Catalog resolution and SQL type-name interpretation retain their existing atomic interfaces.
    pub async fn bind_sequence_references_from_input<F>(
        self,
        resolve: &mut F,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self, SQLError>
    where
        F: FnMut(&str, &ProductionControl<'_>) -> Result<Produced<String>, SQLError> + Send,
    {
        self.bind_sequence_references_from(resolve, control, input)
            .await
    }
}

#[cfg(test)]
mod tests;
