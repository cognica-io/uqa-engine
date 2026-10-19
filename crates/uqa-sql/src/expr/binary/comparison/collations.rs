//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A selected collation is observed only when a non-NULL string comparison reaches it.

use super::{EnumKeyOrder, Ordering, ProductionControl, Result, Value};
use crate::{catalog::collations::BuiltinCollationOrder, expr::SQLValueCatalog};

pub(super) const DEFAULT: u32 = crate::catalog::collations::DEFAULT_COLLATION_OID as u32;

/// Compare already-bound values with the selected collation. Arrays carry it to their elements; record fields select their retained descriptors independently. NULL placement and unreached fields preserve the ordinary comparison's lazy behavior.
pub fn compare_typed_values_with_catalog_and_collation(
    left: &Value,
    right: &Value,
    collation: u32,
    control: &ProductionControl<'_>,
    catalog: Option<&dyn SQLValueCatalog>,
) -> Result<Ordering> {
    super::compare_typed_values_for_purpose(
        left,
        right,
        control,
        catalog,
        EnumKeyOrder::Declaration,
        collation,
    )
}

/// Equality uses the same reached collation diagnostics; every fixed built-in is deterministic, so successful equality retains byte equivalence without computing unused Unicode weights.
pub fn equal_typed_values_with_catalog_and_collation(
    left: &Value,
    right: &Value,
    collation: u32,
    control: &ProductionControl<'_>,
    catalog: Option<&dyn SQLValueCatalog>,
) -> Result<bool> {
    super::equal_values(left, right, control, catalog, true, collation)
}

pub(super) fn compare_text(
    left: &str,
    right: &str,
    collation: u32,
    purpose: EnumKeyOrder,
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    control.check()?;
    let selected = crate::expr::collation::string_comparison_order(collation)?;
    match (selected, purpose) {
        (BuiltinCollationOrder::Unicode, EnumKeyOrder::Declaration) => {
            crate::expr::compare_unicode_with_control(left, right, control)
        }
        _ => uqa_core::compare_bytes_with_control(left.as_bytes(), right.as_bytes(), control)
            .map_err(Into::into),
    }
}
