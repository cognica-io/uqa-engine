//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native value producers retain their existing payload/lease pair until the final public handoff.

use super::Produced;
use crate::{memory::Budgeted, types::OwnedValue, Value};

impl Produced<Value> {
    /// Keep the original value and reservation under iterative native retirement until the next consuming handoff.
    pub fn into_owned_value(self) -> Produced<OwnedValue> {
        Produced {
            value: OwnedValue::new(self.value),
            memory: self.memory,
        }
    }
}

impl Produced<OwnedValue> {
    /// Transfer the original payload and reservation into the public value carrier.
    pub fn into_value(self) -> Produced<Value> {
        Produced {
            value: self.value.into_inner(),
            memory: self.memory,
        }
    }
}

impl Budgeted<Value> {
    /// Keep the original value and reservation under iterative native retirement until the next consuming handoff.
    pub fn into_owned_value(self) -> Budgeted<OwnedValue> {
        let (value, memory) = self.into_parts();
        Budgeted::new(OwnedValue::new(value), memory)
    }
}

impl Budgeted<OwnedValue> {
    /// Transfer the original payload and reservation into the public value carrier.
    pub fn into_value(self) -> Budgeted<Value> {
        let (value, memory) = self.into_parts();
        Budgeted::new(value.into_inner(), memory)
    }
}
