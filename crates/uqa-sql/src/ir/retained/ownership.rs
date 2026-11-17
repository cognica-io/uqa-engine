//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native shared constants transfer their existing values and leases to the final retiring owner.

use super::{ConstantValueSource, RetainedLiteralData, RetainedScalarLiteral};
use std::sync::Arc;
use uqa_core::{
    memory::{MemoryReservation, Produced},
    OwnedValue, ValueRetirement,
};

#[derive(Clone)]
pub(super) enum ConstantSource {
    Native(Arc<Produced<OwnedValue>>),
    External(Arc<dyn ConstantValueSource>),
}

impl ConstantSource {
    pub(super) fn get(&self) -> &dyn ConstantValueSource {
        match self {
            Self::Native(value) => value.as_ref(),
            Self::External(value) => value.as_ref(),
        }
    }
}

pub struct LiteralRetirement {
    value: Option<ValueRetirement>,
    data: RetainedLiteralData,
    value_memory: Option<MemoryReservation>,
    _memory: Option<MemoryReservation>,
}

impl RetainedScalarLiteral {
    pub(crate) fn take_retirement(&mut self) -> Option<LiteralRetirement> {
        let (data, memory) = Arc::into_inner(self.0.take()?)?.into_parts();
        Some(LiteralRetirement {
            value: None,
            data,
            value_memory: None,
            _memory: memory,
        })
    }
}

impl LiteralRetirement {
    pub(crate) fn advance(&mut self) -> bool {
        if let Some(value) = &mut self.value {
            if value.advance() {
                self.value = None;
                self.value_memory = None;
            }
            return false;
        }
        if let Some(source) = self.data.value.source.take() {
            if let ConstantSource::Native(value) = source {
                if let Some(value) = Arc::into_inner(value) {
                    let (owned, memory) = value.into_parts();
                    self.value = Some(owned.into_retirement());
                    self.value_memory = memory;
                }
            }
            return false;
        }
        if let Some(ty) = &mut self.data.ty {
            if ty.retire_step() {
                self.data.ty = None;
            }
            return false;
        }
        self.data.declaration.take().is_none()
    }
}
