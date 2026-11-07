//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Each shared routine value retains its own allocation and native retirement owner.

use crate::{catalog::named_definitions::DefinitionImage, SQLError};
use std::{ops::Deref, sync::Arc};
use uqa_core::memory::{MemoryBudget, MemoryError, MemoryReservation, Produced, ProductionControl};

/// Immutable native routine storage. A body can outlive its catalog definition without losing its payload allowance or destruction owner.
pub struct RetainedRoutineValue<T> {
    owner: Owner<T>,
}

enum Owner<T> {
    Native(Produced<T>),
    Image {
        value: Box<dyn DefinitionImage<T>>,
        _memory: MemoryReservation,
        bytes: usize,
    },
}

impl<T> Deref for RetainedRoutineValue<T> {
    type Target = T;
    fn deref(&self) -> &T {
        match &self.owner {
            Owner::Native(value) => value,
            Owner::Image { value, .. } => value.definition(),
        }
    }
}

impl<T> RetainedRoutineValue<T> {
    pub fn reserved_bytes(&self) -> usize {
        match &self.owner {
            Owner::Native(value) => value.reserved_bytes(),
            Owner::Image { bytes, .. } => *bytes,
        }
    }

    pub fn is_retained_by(&self, memory: &MemoryBudget) -> bool {
        match &self.owner {
            Owner::Native(value) => value.is_retained_by(memory),
            Owner::Image { value, .. } => value.budget().shares_allowance(memory),
        }
    }

    pub(super) fn native(
        value: Produced<T>,
        control: &ProductionControl<'_>,
    ) -> Result<Arc<Self>, SQLError> {
        let header = control.reserve(size_of::<Self>())?;
        let (value, memory) = value.into_parts();
        let value = control.finish(value, control.combine(memory, header))?;
        Ok(Arc::new(Self {
            owner: Owner::Native(value),
        }))
    }

    pub(crate) fn image<D: DefinitionImage<T> + 'static>(value: D) -> Result<Arc<Self>, SQLError> {
        let header = size_of::<D>()
            .checked_add(size_of::<Self>())
            .ok_or(MemoryError::SizeOverflow)?;
        let bytes = value
            .retained_bytes()?
            .checked_add(header)
            .ok_or(MemoryError::SizeOverflow)?;
        let memory = value.budget().reserve(header)?;
        Ok(Arc::new(Self {
            owner: Owner::Image {
                value: Box::new(value),
                _memory: memory,
                bytes,
            },
        }))
    }
}
