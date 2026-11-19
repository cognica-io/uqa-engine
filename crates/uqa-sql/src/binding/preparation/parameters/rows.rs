//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ROW expansion applies selected fields at their original addresses before moving them into the existing row constructor.

use super::{AnalysisVec, OwnedScalarExpr, ProductionControl, RowExpansion, SQLError, ScalarExpr};
use crate::{
    plan::{ScalarEdit, ScalarInputWork},
    schema::retention::CatalogRetentionError,
};

pub(super) async fn expand_from<W: ScalarInputWork, E: ScalarEdit<W>>(
    expression: OwnedScalarExpr,
    expanded: RowExpansion,
    control: &ProductionControl<'_>,
    work: &mut W,
    edit: &mut E,
) -> Result<OwnedScalarExpr, SQLError>
where
    W::Error: Into<SQLError> + From<CatalogRetentionError>,
{
    if !matches!(&*expression, ScalarExpr::Row(_)) {
        return Err(SQLError::Internal(
            "analyzed ROW expansion lost its constructor".into(),
        ));
    }
    let mut extra = 0;
    for (_, fields) in &expanded {
        control.check()?;
        work.checkpoint().await.map_err(Into::into)?;
        control.check()?;
        extra += fields.len();
    }
    let mut selected = AnalysisVec::new(control);
    selected.reserve(
        expression.child_count().saturating_sub(expanded.len()) + extra,
        control,
    )?;
    let mut expanded = expanded.into_iter().peekable();
    let mut children = expression.into_children();
    for position in 0..children.len() {
        let item = children
            .next_from(control, work)
            .await
            .map_err(Into::into)?
            .expect("original ROW child");
        if expanded.peek().is_some_and(|(index, _)| *index == position) {
            let (_, fields) = expanded.next().expect("selected ROW expansion");
            // The original field vector precedes its lease on every exit, including suspension inside a field edit.
            let mut fields = fields.into_parts();
            for field in &mut fields.0 {
                field.edit_retained_from(control, work, edit).await?;
            }
            let fields = control.finish(fields.0, fields.1)?;
            for field in super::super::ownership::AnalysisItems::from_produced(fields) {
                control.check()?;
                work.checkpoint().await.map_err(Into::into)?;
                control.check()?;
                selected.push(field, control)?;
            }
            item.retire_from(control, work).await.map_err(Into::into)?;
        } else {
            selected.push(item, control)?;
        }
    }
    OwnedScalarExpr::row_from(selected.into_iter(), control, work)
        .await
        .map_err(Into::into)
}

#[cfg(test)]
mod tests;
