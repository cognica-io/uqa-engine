//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collection membership and individual vector state use separate capture identities.

use super::{native::NativeVectorSources, Contents, LiveIndexes, VectorIndexes};
use crate::{
    memory_capture::{
        MemoryCapture, MemoryCaptureIdentity, MemoryCaptureKey, MemoryCaptureMember,
        MemoryCaptureRegistry, MemoryCaptureSource, MemoryCaptureSources,
    },
    read_control::StorageReadControl,
    vector_index::{image::VectorImageSnapshot, VectorIndex},
    StorageBackendError, StorageBackendResult,
};
use std::{borrow::Borrow, sync::Arc};
use uqa_core::{
    memory::{Budgeted, MemoryBudget, Produced, RetainedMap, RetainedMapSnapshot},
    FieldName,
};

impl VectorIndexes {
    pub fn memory(registry: &MemoryCaptureRegistry) -> Self {
        Self::memory_with_budget(registry, &MemoryBudget::new(usize::MAX))
    }

    /// Construct the native field owner under its own allowance. Captures independently admit its immutable root and cached key footprint.
    pub fn memory_with_budget(registry: &MemoryCaptureRegistry, memory: &MemoryBudget) -> Self {
        Self(Contents::Live {
            indexes: LiveIndexes::new(),
            capture: Some(NativeVectorSources::new(registry, memory)),
        })
    }

    /// Add the exact child identities while constructing a source generation. Later cut registration retains the completed selection without walking this collection.
    pub fn capture_sources(
        &self,
        sources: &MemoryCaptureSources,
    ) -> StorageBackendResult<MemoryCaptureSources> {
        let mut selected = sources.clone();
        if let Contents::Live {
            capture: Some(capture),
            ..
        } = &self.0
        {
            selected = selected.with_sources(&capture.sources)?;
        } else if !self.is_empty() {
            return Err(StorageBackendError::Other(
                "vector owner lacks memory capture admission".into(),
            ));
        }
        Ok(selected)
    }

    /// Borrow the native source-publication adapter before publishing its enclosing table membership. Binding the resulting address is infallible and does not change physical index state.
    pub fn source_publication(&mut self) -> StorageBackendResult<VectorSourcePublication<'_>> {
        match &mut self.0 {
            Contents::Live {
                capture: Some(capture),
                ..
            } => Ok(VectorSourcePublication(capture)),
            _ => Err(StorageBackendError::Other(
                "vector collection has no native source publication".into(),
            )),
        }
    }
}

pub struct VectorSourcePublication<'a>(&'a mut NativeVectorSources);

impl VectorSourcePublication<'_> {
    pub fn sources(&self) -> &MemoryCaptureSources {
        &self.0.sources
    }

    pub fn bind(self, member: MemoryCaptureMember) {
        self.0.member = Some(member);
    }
}

/// Mutation access cannot replace a registered owner without preserving its selected cuts. New owners join the same domain before collection publication; rejected registration leaves the existing collection unchanged.
pub struct LiveVectorIndexesMut<'a> {
    pub(super) indexes: &'a mut LiveIndexes,
    pub(super) capture: &'a mut Option<NativeVectorSources>,
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
        let index = match self.capture.as_ref() {
            Some(capture) => index.into_memory_capture(&capture.registry)?,
            None => index,
        };
        let capture = self
            .capture
            .as_ref()
            .map(|capture| capture.with_index(&field, index.as_ref()))
            .transpose()?;
        if let (Some(previous), Some(next)) = (self.capture.as_ref(), &capture) {
            previous.publish_field(&field, next)?;
        }
        if let Some(old) = self.indexes.get(&field) {
            retire(old.as_ref());
        }
        let previous = self.indexes.insert(field, index);
        *self.capture = capture;
        Ok(previous)
    }

    pub fn remove<Q: Ord + ?Sized>(
        &mut self,
        field: &Q,
    ) -> StorageBackendResult<Option<Box<dyn VectorIndex>>>
    where
        FieldName: Borrow<Q>,
    {
        let Some((name, old)) = self.indexes.get_key_value(field) else {
            return Ok(None);
        };
        let capture = self
            .capture
            .as_ref()
            .map(|capture| capture.without_index(name))
            .transpose()?;
        if let (Some(previous), Some(next)) = (self.capture.as_ref(), &capture) {
            previous.publish_field(name, next)?;
        }
        retire(old.as_ref());
        let previous = self.indexes.remove(field);
        *self.capture = capture;
        Ok(previous)
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
        let (indexes, mut capture) = if let Some(previous) = self.capture.as_ref() {
            let mut capture =
                NativeVectorSources::new(&previous.registry, previous.fields.budget());
            let mut registered = LiveIndexes::new();
            for (field, index) in indexes {
                let index = index.into_memory_capture(&capture.registry)?;
                capture = capture.with_index(&field, index.as_ref())?;
                registered.insert(field, index);
            }
            (registered, Some(capture))
        } else {
            (indexes, None)
        };
        if let (Some(previous), Some(next)) = (self.capture.as_ref(), capture.as_mut()) {
            previous.publish_all(next)?;
        }
        for index in self.indexes.values() {
            retire(index.as_ref());
        }
        *self.indexes = indexes;
        *self.capture = capture;
        Ok(())
    }
}

/// Immutable field-to-source identities preserve replaced, renamed and dropped indexes without retaining their mutable owners. Each image is admitted by its own source on first read, first mutation or retirement.
pub struct CapturedVectorIndexes {
    indexes: Budgeted<
        RetainedMapSnapshot<Produced<FieldName>, Budgeted<MemoryCaptureKey<VectorImageSnapshot>>>,
    >,
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
        let indexes = match &self.0 {
            Contents::Live {
                capture: Some(capture),
                ..
            } => capture.fields.capture(control.memory())?,
            _ if self.is_empty() => RetainedMap::new(control.memory()).capture(control.memory())?,
            _ => {
                return Err(StorageBackendError::Other(
                    "vector owner lacks memory capture admission".into(),
                ))
            }
        };
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
        Ok(self
            .indexes
            .iter_bidirectional()
            .map(|(field, _)| field.as_str()))
    }

    /// Resolve a field through its selected source identity without borrowing current membership. A removed owner preserves its image before unpublication; the same field's replacement can never supply bytes for the old identity.
    pub fn image(
        &self,
        field: &str,
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
        key.captured(cut)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector_index::MemoryVectorIndex;

    #[test]
    fn captured_membership_admits_weak_source_storage_before_publication() {
        let registry = MemoryCaptureRegistry::new();
        let source = registry.cell(VectorIndexes::memory(&registry)).unwrap();
        source
            .write()
            .live_mut()
            .unwrap()
            .insert("embedding".into(), Box::new(MemoryVectorIndex::new(2)))
            .unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let cut = registry.capture(&control).unwrap();
        let captured = source.captured(&cut).unwrap();
        for (_, key) in captured.indexes.iter() {
            assert_eq!(key.reserved_bytes(), key.retained_source_bytes());
            assert!(key.retained_source_bytes() > 0);
            assert!(key.retained(&cut).unwrap().is_none());
        }
        let insufficient = StorageReadControl::with_limit(control.memory().used() - 1);
        let limited = registry.capture(&insufficient).unwrap();
        assert!(matches!(
            source.captured(&limited),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(source.read()["embedding"].count().unwrap(), 0);
        assert!(cut.check().is_ok());
        drop((source, registry, limited));
        assert_eq!(insufficient.memory().used(), 0);
        drop(cut);
        assert!(control.memory().used() > 0);
        drop(captured);
        assert_eq!(control.memory().used(), 0);
    }
}
