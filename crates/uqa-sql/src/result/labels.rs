//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Host-facing results carry enum values as their current labels, as `PostgreSQL` clients receive them through the type's output function.

use super::SQLResult;
use crate::expr::enums::{contains_enum_carrier, render_enum_labels, EnumLabelCatalog};
use crate::SQLError;

/// Replace every enum carrier in the result's named and positional rows by its label text. Declared column types keep the enum type.
pub fn render_result_enum_labels(
    catalog: Option<&dyn EnumLabelCatalog>,
    result: &mut SQLResult,
) -> Result<(), SQLError> {
    for row in &mut result.rows {
        for value in row.values_mut() {
            if contains_enum_carrier(value) {
                *value = render_enum_labels(catalog, value)?;
            }
        }
    }
    for row in result.positional_rows.iter_mut().flatten() {
        for value in row {
            if contains_enum_carrier(value) {
                *value = render_enum_labels(catalog, value)?;
            }
        }
    }
    Ok(())
}
