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
            Some(oid) => crate::expr::type_by_oid_with_control(oid, catalog, control)?,
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
