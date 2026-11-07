//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable reconstructed entries retain their native destruction owner with the catalog root.

use super::{DefinitionKey, DefinitionRegistry, StoredDefinition};
use crate::SQLError;
use std::ops::Deref;
use uqa_core::memory::{Budgeted, MemoryBudget, MemoryError, MemoryReservation};

pub(crate) trait DefinitionImage<T>: Send + Sync {
    fn definition(&self) -> &T;
    fn budget(&self) -> &MemoryBudget;
    fn retained_bytes(&self) -> Result<usize, MemoryError>;
}

pub(super) enum DefinitionEntry<T> {
    Native(Budgeted<T>),
    Image {
        value: Box<dyn DefinitionImage<T>>,
        _memory: MemoryReservation,
    },
}

impl<T> Deref for DefinitionEntry<T> {
    type Target = T;
    fn deref(&self) -> &T {
        match self {
            Self::Native(value) => value,
            Self::Image { value, .. } => value.definition(),
        }
    }
}

impl<K: DefinitionKey, T: StoredDefinition<K::View>> DefinitionRegistry<K, T> {
    pub(crate) fn insert_owned_image<D: DefinitionImage<T> + 'static>(
        &mut self,
        name: Budgeted<K>,
        value: D,
    ) -> Result<(), SQLError> {
        assert!(self.budget().shares_allowance(name.budget()));
        assert!(self.budget().shares_allowance(value.budget()));
        let bytes = name
            .payload_bytes()
            .checked_add(value.retained_bytes()?)
            .and_then(|bytes| bytes.checked_add(size_of::<D>()))
            .ok_or(MemoryError::SizeOverflow)?;
        let mut memory = self.budget().reserve(size_of::<D>())?;
        let (name, key_memory) = name.into_parts();
        memory.absorb(key_memory);
        self.publish_entry(
            name,
            DefinitionEntry::Image {
                value: Box::new(value),
                _memory: memory,
            },
            bytes,
        )
    }
}
