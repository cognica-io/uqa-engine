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
