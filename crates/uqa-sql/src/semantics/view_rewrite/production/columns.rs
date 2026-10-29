//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    OwnedScalarExpr, OwnedViewColumn, Produced, ProductionControl, ProductionMap, ProductionVec,
    RowSchema, SQLError, ScalarExpr,
};

#[expect(
    clippy::too_many_arguments,
    reason = "one view layer supplies the selected source and output namespaces"
)]
pub(super) fn produce(
    block: &crate::plan::QueryBlockPlan,
    source_name: &str,
    qualifier: &str,
    source_columns: &[String],
    aliases: &[String],
    mapping: &ProductionMap<'_, &str, &str>,
    output: &RowSchema,
    canonical: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<OwnedViewColumn>>, SQLError> {
    let mut columns = ProductionVec::new(*control);
    for projection in &block.projections {
        control.check()?;
        let star_scope = match &projection.expr {
            ScalarExpr::Star => Some(false),
            ScalarExpr::QualifiedStar(star)
                if super::super::source_qualifier_matches(star, qualifier, source_name) =>
            {
                Some(true)
            }
            _ => None,
        };
        if let Some(is_qualified) = star_scope {
            for (position, name) in source_columns.iter().enumerate() {
                let name = aliases.get(position).unwrap_or(name);
                let expression = OwnedScalarExpr::column(
                    control.copy_text(name)?,
                    is_qualified
                        .then(|| control.copy_text(qualifier))
                        .transpose()?,
                    control,
                )?;
                append(
                    &mut columns,
                    expression,
                    source_name,
                    qualifier,
                    mapping,
                    output,
                    control,
                )?;
            }
        } else {
            append(
                &mut columns,
                projection.expr.copy_for_rewrite(control)?,
                source_name,
                qualifier,
                mapping,
                output,
                control,
            )?;
        }
    }
    if columns.len() != output.len() {
        return Err(SQLError::Internal(format!(
            "view `{canonical}` has {} stored projections for {} output columns",
            columns.len(),
            output.len()
        )));
    }
    Ok(columns.finish()?)
}

fn append(
    columns: &mut ProductionVec<'_, OwnedViewColumn>,
    expression: OwnedScalarExpr,
    source: &str,
    qualifier: &str,
    mapping: &ProductionMap<'_, &str, &str>,
    output: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let name = match &*expression {
        ScalarExpr::Column(column) => Some(column.as_str()),
        ScalarExpr::QualifiedColumn {
            qualifier: actual,
            column,
        } if super::super::source_qualifier_matches(actual, qualifier, source) => {
            Some(column.as_str())
        }
        _ => None,
    }
    .and_then(|column| mapping.get(column).copied());
    let position = columns.len();
    // A malformed stored width is reported after visiting every projection, as in the ordinary producer.
    let output_name = output
        .columns()
        .get(position)
        .map(String::as_str)
        .unwrap_or("");
    let column = OwnedViewColumn {
        name: control.copy_text(output.public_name(position).unwrap_or(output_name))?,
        collation_oid: output.column_collation(position),
        writable_source_column: name.map(|name| control.copy_text(name)).transpose()?,
        expression,
    };
    columns.push_produced(control.finish(column, control.empty_reservation())?)?;
    Ok(())
}
