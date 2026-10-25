//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Anonymous record output uses admitted field identities, including physical nested values.

use super::{
    append_quoted, format_postgres_text_with_catalog_and_control, ColumnType, Produced,
    ProductionControl, ProductionString, SQLError, SQLValueCatalog, Value,
};
use crate::catalog::type_metadata::{builtin_array_element, builtin_scalar_type};

pub(super) fn format_row(
    row: &uqa_core::RowValue,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    let mut text = ProductionString::new(*control);
    text.push('(')?;
    for (index, field) in row.iter().enumerate() {
        control.check()?;
        if index != 0 {
            text.push(',')?;
        }
        if matches!(field, Value::Null) {
            continue;
        }
        let oid = row
            .field_types()
            .map(|types| types[index].oid)
            .filter(|oid| *oid != 705)
            .or_else(|| {
                if let Value::Datum(datum) = field {
                    Some(datum.type_oid())
                } else {
                    None
                }
            });
        let ty = match oid {
            Some(oid) => output_type(oid, catalog, control)?,
            None => crate::type_resolution::value_type_with_control(field, control)?
                .unwrap_or(control.finish(ColumnType::Text, control.empty_reservation())?),
        };
        let rendered = format_postgres_text_with_catalog_and_control(field, &ty, catalog, control)?;
        let quoted = rendered.is_empty()
            || rendered
                .chars()
                .any(|c| c.is_ascii_whitespace() || matches!(c, ',' | '(' | ')' | '"' | '\\'));
        append_quoted(&mut text, &rendered, quoted, true)?;
    }
    text.push(')')?;
    Ok(text.finish()?)
}

// Output selects the field's type output function; it never reapplies typmods or input/domain checks.
fn output_type(
    oid: u32,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    if let Some(ty) = builtin_scalar_type(oid) {
        return Ok(ty.clone_with_control(control)?);
    }
    if let Some(element) = builtin_array_element(oid) {
        return Ok(ColumnType::array_with_control(
            element.clone_with_control(control)?,
            control,
        )?);
    }
    catalog
        .map(|catalog| catalog.value_type_by_oid(oid))
        .transpose()?
        .flatten()
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "XX000".into(),
            message: format!("cache lookup failed for type {oid}"),
        })?
        .retain_external_with_control(control)
        .map_err(Into::into)
}
