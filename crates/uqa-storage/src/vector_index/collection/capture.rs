//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collection membership and individual vector state use separate capture identities.

use super::{copy_field, Contents, LiveIndexes, VectorIndexes};
use crate::{
    memory_capture::{
        MemoryCapture, MemoryCaptureIdentity, MemoryCaptureKey, MemoryCaptureRegistry,
        MemoryCaptureSource,
    },
    read_control::StorageReadControl,
    vector_index::{image::VectorImageSnapshot, VectorIndex},
    StorageBackendError, StorageBackendResult,
};
use std::{borrow::Borrow, sync::Arc};
use uqa_core::{
    memory::{Budgeted, BudgetedMap},
    FieldName,
};

impl VectorIndexes {
    pub fn memory(registry: &MemoryCaptureRegistry) -> Self {
        Self(Contents::Live {
            indexes: LiveIndexes::new(),
            registry: Some(registry.clone()),
        })
    }
}

/// Mutation access cannot replace a registered owner without preserving its selected cuts. New owners join the same domain before collection publication; rejected registration leaves the existing collection unchanged.
pub struct LiveVectorIndexesMut<'a> {
    pub(super) indexes: &'a mut LiveIndexes,
    pub(super) registry: &'a Option<MemoryCaptureRegistry>,
}

fn retire(index: &dyn VectorIndex) {
    if let Some(capture) = index.memory_capture() {
        capture.retire();
    }
}

impl LiveVectorIndexesMut<'_> {
    pub fn insert(
        &mut self,
        field: FieldName,
        index: Box<dyn VectorIndex>,
    ) -> StorageBackendResult<Option<Box<dyn VectorIndex>>> {
        let index = match self.registry {
            Some(registry) => index.into_memory_capture(registry)?,
            None => index,
        };
        if let Some(old) = self.indexes.get(&field) {
            retire(old.as_ref());
        }
        Ok(self.indexes.insert(field, index))
    }

    pub fn remove<Q: Ord + ?Sized>(&mut self, field: &Q) -> Option<Box<dyn VectorIndex>>
    where
        FieldName: Borrow<Q>,
    {
        if let Some(old) = self.indexes.get(field) {
            retire(old.as_ref());
        }
        self.indexes.remove(field)
    }

    pub fn get_mut<Q: Ord + ?Sized>(
        &mut self,
        field: &Q,
    ) -> Option<&mut (dyn VectorIndex + 'static)>
    where
        FieldName: Borrow<Q>,
    {
        self.indexes.get_mut(field).map(Box::as_mut)
    }

    pub fn values_mut(
        &mut self,
    ) -> impl ExactSizeIterator<Item = &mut (dyn VectorIndex + 'static)> {
        self.indexes.values_mut().map(Box::as_mut)
    }

    pub fn iter_mut(
        &mut self,
    ) -> impl ExactSizeIterator<Item = (&FieldName, &mut (dyn VectorIndex + 'static))> {
        self.indexes
            .iter_mut()
            .map(|(field, index)| (field, index.as_mut()))
    }

    pub fn replace(&mut self, indexes: LiveIndexes) -> StorageBackendResult<()> {
        let indexes = match self.registry {
            Some(registry) => indexes
                .into_iter()
                .map(|(field, index)| {
                    index
                        .into_memory_capture(registry)
                        .map(|index| (field, index))
                })
                .collect::<StorageBackendResult<LiveIndexes>>()?,
            None => indexes,
        };
        for index in self.indexes.values() {
            retire(index.as_ref());
        }
        *self.indexes = indexes;
        Ok(())
    }
}

/// Immutable field-to-source identities preserve replaced, renamed and dropped indexes without retaining their mutable owners. Each image is admitted by its own source on first read, first mutation or retirement.
pub struct CapturedVectorIndexes {
    indexes: BudgetedMap<FieldName, Budgeted<MemoryCaptureKey<VectorImageSnapshot>>>,
    control: StorageReadControl,
    identity: MemoryCaptureIdentity,
}

impl MemoryCaptureSource for VectorIndexes {
    type Frozen = CapturedVectorIndexes;
    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        control.check()?;
        Err(StorageBackendError::Other(
            "vector membership capture requires its original cut".into(),
        ))
    }
    fn freeze_at(
        &self,
        control: &StorageReadControl,
        identity: &MemoryCaptureIdentity,
    ) -> StorageBackendResult<Self::Frozen> {
        control.check()?;
        let mut indexes = BudgetedMap::new(control.memory());
        for (field, index) in self {
            let capture = index.memory_capture().ok_or_else(|| {
                StorageBackendError::Other("vector owner lacks memory capture admission".into())
            })?;
            let (field, memory) = copy_field(field, control)?;
            indexes.insert(field, Budgeted::new(capture.key()?, memory))?;
        }
        control.check()?;
        Ok(CapturedVectorIndexes {
            indexes,
            control: control.clone(),
            identity: identity.clone(),
        })
    }
}

impl CapturedVectorIndexes {
    pub fn fields(&self) -> StorageBackendResult<impl ExactSizeIterator<Item = &str>> {
        self.control.check()?;
        Ok(self.indexes.iter().map(|(field, _)| field.as_str()))
    }

    /// Resolve a field using its retained identity before consulting current membership. A removed owner preserves its image before unpublication; the same field's replacement can never supply bytes for the old identity.
    pub fn image(
        &self,
        field: &str,
        current: Option<&VectorIndexes>,
        cut: &MemoryCapture,
    ) -> StorageBackendResult<Arc<Budgeted<VectorImageSnapshot>>> {
        self.control.check()?;
        cut.check()?;
        if !self.identity.matches(cut) {
            return Err(StorageBackendError::Other(
                "vector membership belongs to another memory capture".into(),
            ));
        }
        let key = self.indexes.get(field).ok_or_else(|| {
            StorageBackendError::Other("vector field is absent from captured membership".into())
        })?;
        if let Some(image) = key.retained(cut)? {
            return Ok(image);
        }
        if let Some(index) = current
            .and_then(|current| current.get(field))
            .and_then(VectorIndex::memory_capture)
        {
            if key.same_source(&index.key()?) {
                return index.captured(cut);
            }
        }
        // Current membership may have changed while an exporter acquired its outer read guard. Retirement has already published the old image by that point.
        key.retained(cut)?.ok_or_else(|| {
            StorageBackendError::Other(
                "captured vector source disappeared without retirement".into(),
            )
        })
    }
}
