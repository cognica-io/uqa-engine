//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected membership coercions and left operands share their original order across immediate and suspended input.

use super::{MembershipAnalysis, OwnedScalarExpr, ProductionControl, SQLError, ScalarExpr};
use crate::{
    plan::{retirement::checkpoint, ScalarInputWork},
    schema::retention::CatalogRetentionError,
};
use std::future::{ready, Future};

pub(super) trait Occurrences<W: ScalarInputWork> {
    fn copy(
        &mut self,
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> impl Future<Output = Result<OwnedScalarExpr, SQLError>> + Send;
}

pub(super) struct Immediate<'a, F>(pub(super) &'a mut F);

impl<F: FnMut(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>> Occurrences<()>
    for Immediate<'_, F>
{
    fn copy(
        &mut self,
        expression: OwnedScalarExpr,
        _: &ProductionControl<'_>,
        (): &mut (),
    ) -> impl Future<Output = Result<OwnedScalarExpr, SQLError>> + Send {
        ready((self.0)(expression))
    }
}

impl MembershipAnalysis {
    pub(super) async fn apply_from<W: ScalarInputWork, I: Occurrences<W>>(
        self,
        mut expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut W,
        occurrences: &mut I,
    ) -> Result<OwnedScalarExpr, SQLError>
    where
        W::Error: From<SQLError> + From<CatalogRetentionError> + Into<SQLError>,
    {
        checkpoint(control, work).await.map_err(Into::into)?;
        if !matches!(&*expression, ScalarExpr::InList { .. }) {
            return Err(SQLError::Internal(
                "retained membership no longer matches its syntax".into(),
            ));
        }
        for (index, target) in self.array_coercions {
            checkpoint(control, work).await.map_err(Into::into)?;
            expression = expression
                .map_child_from(index + 1, control, |item| async {
                    let item = OwnedScalarExpr::cast(
                        item,
                        work.type_name(&target, control).await.map_err(Into::into)?,
                        true,
                        control,
                    )?;
                    target
                        .retire_from(control, work)
                        .await
                        .map_err(Into::into)?;
                    Ok::<_, SQLError>(item)
                })
                .await?;
        }
        expression = crate::type_resolution::membership::rewrite_owned_from(
            expression,
            &self.array_items,
            control,
            work,
        )
        .await
        .map_err(Into::into)?;
        if self.left_constants.is_empty() {
            return Ok(expression);
        }
        let multiple = matches!(&*expression, ScalarExpr::And(_) | ScalarExpr::Or(_));
        let count = if multiple {
            expression.child_count()
        } else {
            1
        };
        if count != self.left_constants.len() {
            return Err(SQLError::Internal(
                "analyzed membership comparison count changed".into(),
            ));
        }
        let last = count.saturating_sub(1);
        for (index, constant) in self.left_constants.into_iter().enumerate() {
            checkpoint(control, work).await.map_err(Into::into)?;
            let apply = |comparison: OwnedScalarExpr| async {
                comparison
                    .map_child_from(0, control, |left| async {
                        let left = match constant {
                            Some(constant) => {
                                left.retire_from(control, work).await.map_err(Into::into)?;
                                constant.expression
                            }
                            None => left,
                        };
                        if index < last {
                            occurrences.copy(left, control, work).await
                        } else {
                            Ok(left)
                        }
                    })
                    .await
            };
            expression = if multiple {
                expression.map_child_from(index, control, apply).await?
            } else {
                apply(expression).await?
            };
        }
        Ok(expression)
    }
}

#[cfg(test)]
mod tests;

/// Stored syntax keeps each occurrence paired with its written query. The executable binder can instead allocate an independent initialization slot for each occurrence.
struct WrittenQueries(usize);

impl<W: ScalarInputWork> crate::plan::QueryIndexInput<W> for WrittenQueries {
    async fn index(
        &mut self,
        index: usize,
        _: &ProductionControl<'_>,
        _: &mut W,
    ) -> Result<usize, SQLError> {
        if index < self.0 {
            Ok(index)
        } else {
            Err(SQLError::Internal(
                "copied scalar subquery is outside its owning arena".into(),
            ))
        }
    }
}

impl<W: ScalarInputWork> Occurrences<W> for WrittenQueries
where
    W::Error: Into<SQLError>,
{
    async fn copy(
        &mut self,
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<OwnedScalarExpr, SQLError> {
        expression
            .remap_query_occurrences_from(control, work, self)
            .await
    }
}

impl MembershipAnalysis {
    pub(in crate::binding::preparation) async fn apply_stored_from<W: ScalarInputWork>(
        self,
        expression: OwnedScalarExpr,
        width: usize,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<OwnedScalarExpr, SQLError>
    where
        W::Error: From<SQLError> + From<CatalogRetentionError> + Into<SQLError>,
    {
        self.apply_from(expression, control, work, &mut WrittenQueries(width))
            .await
    }
}
