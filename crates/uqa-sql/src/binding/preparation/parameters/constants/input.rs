//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Node input application keeps the original selection order while nested ROW fields reuse the same scalar editor.

use super::{
    AnalysisIdentity, InputConstants, OwnedScalarExpr, ProductionControl, SQLError, ScalarExpr,
};
use crate::{
    plan::{
        retirement::{checkpoint, retire_produced_from},
        ScalarEdit, ScalarEditIdentity, ScalarInputWork,
    },
    schema::retention::CatalogRetentionError,
};

pub(in crate::binding::preparation::parameters) trait Application<W: ScalarInputWork>:
    ScalarEdit<W, Identity = AnalysisIdentity<ScalarExpr>>
{
    fn constants(&mut self) -> &mut InputConstants;
}

pub(in crate::binding::preparation::parameters) async fn apply_from<
    W: ScalarInputWork,
    E: Application<W>,
>(
    edit: &mut E,
    identity: AnalysisIdentity<ScalarExpr>,
    mut expression: OwnedScalarExpr,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<OwnedScalarExpr, SQLError>
where
    W::Error: From<SQLError> + From<CatalogRetentionError> + Into<SQLError>,
{
    checkpoint(control, work).await.map_err(Into::into)?;
    if let Some(index) = edit.constants().11.remove(&identity) {
        expression
            .retire_from(control, work)
            .await
            .map_err(Into::into)?;
        expression = ScalarExpr::Param(index).copy_for_rewrite(control)?;
    }
    if let Some(expanded) = edit.constants().9.remove(&identity) {
        expression =
            super::super::rows::expand_from(expression, expanded, control, work, edit).await?;
    }
    if let Some(selected) = edit.constants().1.remove(&identity) {
        if matches!(&*expression, ScalarExpr::Func { .. }) {
            expression = expression
                .replace_call_binding_from(Some(selected), control, work)
                .await
                .map_err(Into::into)?;
            expression = crate::type_resolution::json_path::normalize_retained_owned_from(
                expression, control, work,
            )
            .await
            .map_err(Into::into)?;
        } else {
            retire_produced_from(selected, control, work)
                .await
                .map_err(Into::into)?;
        }
    }
    edit.constants()
        .apply_selected_inputs_from(identity, expression, control, work)
        .await
        .map_err(Into::into)
}

pub(super) struct Inline<'a>(pub(super) &'a mut InputConstants);

impl ScalarEditIdentity for Inline<'_> {
    type Identity = AnalysisIdentity<ScalarExpr>;
    const BEFORE_CHILDREN: bool = false;
    fn identity(expression: &ScalarExpr) -> Self::Identity {
        expression.into()
    }
}

impl ScalarEdit<()> for Inline<'_> {
    fn apply(
        &mut self,
        identity: Self::Identity,
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        (): &mut (),
    ) -> impl std::future::Future<Output = Result<OwnedScalarExpr, SQLError>> + Send {
        std::future::ready(self.0.apply_node_owned(identity, expression, control))
    }
}

impl Application<()> for Inline<'_> {
    fn constants(&mut self) -> &mut InputConstants {
        self.0
    }
}

use crate::construction::{self, Read, Work};

type AppliedInput = Result<(OwnedScalarExpr, InputConstants), SQLError>;

/// A nested replacement transfers its original position table to the selected child and takes it back on completion.
pub(in crate::binding::preparation::parameters) struct Selected {
    constants: Option<InputConstants>,
    arena: Option<usize>,
}
impl Selected {
    pub(in crate::binding::preparation::parameters) fn new(constants: InputConstants) -> Self {
        Self {
            constants: Some(constants),
            arena: None,
        }
    }
    pub(in crate::binding::preparation::parameters) fn into_constants(mut self) -> InputConstants {
        self.constants
            .take()
            .expect("completed reference edit owns its positions")
    }
    pub(in crate::binding::preparation::parameters) fn set_arena(&mut self, width: Option<usize>) {
        self.arena = width;
    }
    pub(in crate::binding::preparation::parameters) fn inputs(&mut self) -> &mut InputConstants {
        self.constants
            .as_mut()
            .expect("active scalar editor owns its positions")
    }
}
impl ScalarEditIdentity for Selected {
    type Identity = AnalysisIdentity<ScalarExpr>;
    const BEFORE_CHILDREN: bool = false;
    fn identity(expression: &ScalarExpr) -> Self::Identity {
        expression.into()
    }
}
impl Application<Work<'_>> for Selected {
    fn constants(&mut self) -> &mut InputConstants {
        self.inputs()
    }
}
impl<'a> ScalarEdit<Work<'a>> for Selected {
    async fn apply(
        &mut self,
        identity: Self::Identity,
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut Work<'a>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let constants = self
            .constants
            .take()
            .expect("parent transfers input selections");
        let arena = self.arena;
        let owner = control.capture();
        let output = work.submit(move |work| async move {
            apply_node(constants, arena, identity, expression, owner, work)?.await
        })?;
        let (expression, constants) = construction::receive(output).await?;
        self.constants = Some(constants);
        Ok(expression)
    }
}

pub(in crate::binding::preparation::parameters) fn apply_node(
    constants: InputConstants,
    arena: Option<usize>,
    identity: AnalysisIdentity<ScalarExpr>,
    expression: OwnedScalarExpr,
    owner: uqa_core::memory::OwnedProductionControl,
    mut work: Work<'_>,
) -> Result<Read<'_, AppliedInput>, SQLError> {
    let admission = owner.clone();
    Ok(Read::new(admission.control(), async move {
        let control = owner.control();
        let mut edit = Selected::new(constants);
        edit.set_arena(arena);
        let expression = match (arena, edit.inputs().2.remove(&identity)) {
            (Some(width), Some(membership)) => {
                construction::retain_with(control, || {
                    membership.apply_stored_from(expression, width, &control, &mut work)
                })?
                .await?
            }
            (_, None) => expression,
            (None, Some(_)) => {
                return Err(SQLError::Internal(
                    "stored membership has no owning query arena".into(),
                ))
            }
        };
        let output = Read::new(
            control,
            apply_from(&mut edit, identity, expression, &control, &mut work),
        )?
        .await?;
        Ok((
            output,
            edit.constants
                .take()
                .expect("completed node owns its input selections"),
        ))
    })?)
}
