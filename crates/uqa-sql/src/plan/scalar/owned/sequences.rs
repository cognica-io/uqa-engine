//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored sequence edits retain the selected name's original owner and preserve surrounding call and cast metadata.

use super::{
    traversal::{Edit, EditIdentity},
    OwnedScalarExpr, ScalarExpr,
};
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

pub(in crate::plan) struct Bind<F>(pub(in crate::plan) F);

impl<F> EditIdentity for Bind<F> {
    type Identity = ();
    const BEFORE_CHILDREN: bool = false;
    fn identity(_: &ScalarExpr) {}
}

impl<F, W: Work> Edit<W> for Bind<F>
where
    F: FnMut(&str, &ProductionControl<'_>) -> Result<Produced<String>, SQLError> + Send,
    W::Error: Into<SQLError>,
{
    async fn apply(
        &mut self,
        (): (),
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<OwnedScalarExpr, SQLError> {
        if !matches!(&*expression, ScalarExpr::Func { name, args, .. }
            if !args.is_empty() && is_sequence_function(name))
        {
            return Ok(expression);
        }
        expression
            .map_child_from(0, control, |argument| async {
                let mut name = Name(&mut self.0);
                let output = crate::construction::retain_with(*control, || {
                    argument.edit_from((), control, work, &mut name)
                })?
                .await;
                output
            })
            .await
    }
}

struct Name<F>(F);

impl<F> EditIdentity for Name<F> {
    type Identity = ();
    const BEFORE_CHILDREN: bool = false;
    fn identity(_: &ScalarExpr) {}

    fn child_count(expression: &ScalarExpr) -> usize {
        usize::from(matches!(expression, ScalarExpr::Cast { ty, .. } if is_regclass_cast(ty)))
    }
}

impl<F, W: Work> Edit<W> for Name<F>
where
    F: FnMut(&str, &ProductionControl<'_>) -> Result<Produced<String>, SQLError> + Send,
    W::Error: Into<SQLError>,
{
    async fn apply(
        &mut self,
        (): (),
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        _work: &mut W,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let ScalarExpr::Literal(Value::Str(reference)) = &*expression else {
            return Ok(expression);
        };
        let (name, reservation) = (self.0)(reference, control)?.into_parts();
        Ok(OwnedScalarExpr::literal(
            control.finish(Value::Str(name), reservation)?,
            control,
        )?)
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
