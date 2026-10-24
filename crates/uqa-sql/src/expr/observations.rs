//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inspect deferred and catalog-dependent value carriers at output boundaries.

use uqa_core::Value;

#[derive(Clone, Copy, Default)]
pub(crate) struct OutputCarriers {
    pub(crate) datum: bool,
    pub(crate) enumeration: bool,
}

/// Inspect both carrier kinds without decoding values or querying the catalog.
pub(crate) fn output_carriers(value: &Value) -> OutputCarriers {
    let mut found = OutputCarriers::default();
    any_nested_value(value, &mut |value| {
        found.datum |= matches!(value, Value::Datum(_));
        found.enumeration |= matches!(value, Value::Enum(_));
        // Decode the first Datum before inspecting any later value, as the
        // original Datum-presence check did. Decoding may itself fail.
        found.datum
    });
    found
}

/// Walk native carrier children in their stored order, stopping at the first match.
pub(crate) fn any_nested_value(value: &Value, predicate: &mut impl FnMut(&Value) -> bool) -> bool {
    #[cfg(test)]
    record_visit();
    if predicate(value) {
        return true;
    }
    match value {
        Value::Array(array) => array
            .elements()
            .iter()
            .any(|value| any_nested_value(value, predicate)),
        Value::List(values) => values
            .iter()
            .any(|value| any_nested_value(value, predicate)),
        Value::Row(values) => values
            .iter()
            .any(|value| any_nested_value(value, predicate)),
        Value::Record(fields) => fields
            .values()
            .iter()
            .any(|value| any_nested_value(value, predicate)),
        Value::Map(values) => values
            .values()
            .any(|value| any_nested_value(value, predicate)),
        _ => false,
    }
}

#[cfg(test)]
thread_local! {
    static VALUE_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn record_visit() {
    VALUE_VISITS.with(|visits| visits.set(visits.get() + 1));
}

#[cfg(test)]
pub(crate) fn visits() -> usize {
    VALUE_VISITS.with(std::cell::Cell::get)
}
