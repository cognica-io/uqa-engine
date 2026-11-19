//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Observe the scalar representations enabled by the catalog's public JSON dependency.

pub(crate) use uqa_core::json::JsonScalarProtocol as Keys;
use uqa_core::memory::ProductionControl;
use uqa_storage::StorageBackendResult;

pub(crate) fn keys(control: &ProductionControl<'_>) -> StorageBackendResult<Keys> {
    control.check()?;
    let _scratch = control.reserve(128)?;
    Ok(Keys::from_values(
        &serde_json::Number::from(1u8),
        serde_json::value::RawValue::NULL,
    )?)
}
