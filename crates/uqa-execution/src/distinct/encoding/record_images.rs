//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact keys for the storage-image equality selected by a record operator class.

use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};
use uqa_sql::{expr::SQLValueCatalog, SQLError};

/// Produce an exact key for an admitted record image within its selected field-identity domain. SQL owns field observation; Execution frames identities, NULLs, byte lengths and arity. These keys do not replace ordinary value-equality keys or define an order-preserving encoding.
pub fn record_image_key_with_catalog_and_control(
    value: &Value,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>, SQLError> {
    control.check()?;
    let mut output = ProductionVec::new(*control);
    let count = uqa_sql::expr::visit_record_image_fields_with_catalog(
        value,
        catalog,
        control,
        |oid, bytes| {
            output.extend_from_slice(&oid.unwrap_or(0).to_le_bytes())?;
            output.push_copy(u8::from(bytes.is_some()))?;
            if let Some(bytes) = bytes {
                super::encode_bytes(bytes, &mut output).map_err(super::sql_error)?;
            }
            Ok(())
        },
    )?;
    super::encode_len(count, &mut output).map_err(super::sql_error)?;
    Ok(output.finish()?)
}

#[cfg(test)]
mod tests;
