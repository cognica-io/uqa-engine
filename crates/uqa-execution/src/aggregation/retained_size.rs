//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained aggregate input and group-key storage.

use uqa_core::{memory::ProductionControl, Value};

pub(crate) fn value_retained_bytes(value: &Value) -> usize {
    size_of::<Value>().saturating_add(payload_bytes(value))
}

pub(super) fn payload_bytes(value: &Value) -> usize {
    value
        .retained_payload_bytes_with_control(&ProductionControl::uncontrolled())
        .unwrap_or(usize::MAX)
}
