//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admitted `PostgreSQL` `DataRow` payloads for local serialization and protocol consumers.

use crate::{
    expr::{EngineHook, EngineValueCatalog},
    ColumnType, SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultFormat {
    Text,
    Binary,
}

/// Encode the `DataRow` payload (column count, field lengths and field bytes), excluding the transport message header. Only this row and the current field's conversion scratch are retained.
pub fn encode_postgres_row_with_control<'v>(
    fields: impl ExactSizeIterator<Item = (Option<&'v Value>, Option<&'v ColumnType>)> + Clone,
    format: ResultFormat,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>, SQLError> {
    control.check()?;
    if format == ResultFormat::Binary {
        for (_, ty) in fields.clone() {
            control.check()?;
            super::binary::validate_type(ty.unwrap_or(&ColumnType::Text), control)?;
        }
    }
    let mut output = ProductionVec::new(*control);
    output.extend_from_slice(
        &u16::try_from(fields.len())
            .map_err(|_| super::binary::size_error())?
            .to_be_bytes(),
    )?;
    let catalog = engine.map(EngineValueCatalog);
    let catalog = catalog
        .as_ref()
        .map(|catalog| catalog as &dyn crate::expr::SQLValueCatalog);
    for (value, ty) in fields {
        control.check()?;
        let ty = ty.unwrap_or(&ColumnType::Text);
        let Some(value) = value.filter(|value| !matches!(value, Value::Null)) else {
            output.extend_from_slice(&(-1_i32).to_be_bytes())?;
            continue;
        };
        let text;
        let binary;
        let bytes = match format {
            ResultFormat::Text => {
                text = super::format_postgres_text_with_control(value, ty, engine, control)?;
                text.as_bytes()
            }
            ResultFormat::Binary => {
                binary = super::format_postgres_binary_with_catalog_and_control(
                    value, ty, catalog, control,
                )?;
                &binary
            }
        };
        output.extend_from_slice(
            &i32::try_from(bytes.len())
                .map_err(|_| super::binary::size_error())?
                .to_be_bytes(),
        )?;
        output.extend_from_slice(bytes)?;
    }
    Ok(output.finish()?)
}
