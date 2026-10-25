//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read an admitted payload only when an aggregate transition or finalizer reaches it.

use std::borrow::Cow;
use uqa_core::{memory::ProductionControl, Value};
use uqa_sql::{expr::SQLValueCatalog, SQLError};

pub(super) fn read<'a>(
    value: &'a Value,
    catalog: Option<&dyn SQLValueCatalog>,
) -> Result<Cow<'a, Value>, SQLError> {
    if let Value::Datum(datum) = value {
        uqa_sql::expr::datums::read_with_value_catalog_and_control(
            datum,
            catalog,
            &ProductionControl::uncontrolled(),
        )
        .map(|value| Cow::Owned(value.into_uncontrolled().expect("aggregate observation")))
    } else {
        Ok(Cow::Borrowed(value))
    }
}
