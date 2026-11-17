//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Window calls share one pre-order traversal across inline and suspended execution.

use super::{
    CatalogRetentionError, OwnedScalarExpr, ProductionControl, ScalarExpr, ScalarWindowSpec, Work,
};
use crate::{plan::scalar::owned::traversal::Edit, SQLError};

struct Normalize<'a>(&'a [ScalarWindowSpec]);

impl<W: Work> Edit<W> for Normalize<'_>
where
    W::Error: From<CatalogRetentionError> + Into<SQLError> + Send,
{
    type Identity = ();
    const BEFORE_CHILDREN: bool = true;
    fn identity(_: &ScalarExpr) {}

    async fn apply(
        &mut self,
        (): (),
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<OwnedScalarExpr, SQLError> {
        if let ScalarExpr::WindowCall { spec, .. } = &*expression {
            if let Some(slot) = spec.definition {
                let canonical = self.0.get(slot).ok_or_else(|| {
                    crate::plan::windows::invalid("a call refers to a missing window definition")
                })?;
                return expression
                    .replace_window_spec_from(canonical, control, work)
                    .await
                    .map_err(Into::into);
            }
        }
        Ok(expression)
    }
}

impl OwnedScalarExpr {
    pub(in crate::plan) async fn normalize_window_definitions_from<W: Work>(
        self,
        resolved: &[ScalarWindowSpec],
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Self, SQLError>
    where
        W::Error: From<CatalogRetentionError> + Into<SQLError> + Send,
    {
        self.edit_from((), control, work, &mut Normalize(resolved))
            .await
    }
}
