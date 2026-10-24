//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inspect deferred and catalog-dependent value carriers at output boundaries.

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
