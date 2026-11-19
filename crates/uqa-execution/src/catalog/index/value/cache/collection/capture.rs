//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A captured name selects its original evaluated child through the same exact cut.

use super::{
    native::{IndexSource, NativeValueSources},
    CapturedValueIndex, ValueIndexes,
};
use std::sync::Arc;
use uqa_core::memory::{Budgeted, RetainedMap, RetainedMapSnapshot};
use uqa_storage::{
    memory_capture::{
        MemoryCapture, MemoryCaptureIdentity, MemoryCaptureMember, MemoryCaptureSource,
        MemoryCaptureSources,
    },
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult, ValueIndexKey,
};

impl ValueIndexes {
    pub fn capture_sources(
        &self,
        sources: &MemoryCaptureSources,
    ) -> StorageBackendResult<MemoryCaptureSources> {
        match &self.native {
            Some(native) => sources.with_sources(&native.sources),
            None if self.is_empty() => Ok(sources.clone()),
            None => Err(unregistered()),
        }
    }

    pub fn source_publication(&mut self) -> StorageBackendResult<ValueSourcePublication<'_>> {
        self.native
            .as_mut()
            .map(ValueSourcePublication)
            .ok_or_else(unregistered)
    }
}

pub struct ValueSourcePublication<'a>(&'a mut NativeValueSources);

impl ValueSourcePublication<'_> {
    pub fn sources(&self) -> &MemoryCaptureSources {
        &self.0.sources
    }
    pub fn bind(self, member: MemoryCaptureMember) {
        self.0.member = Some(member);
    }
}

pub struct CapturedValueIndexes {
    indexes: Budgeted<RetainedMapSnapshot<ValueIndexKey, Budgeted<IndexSource>>>,
    control: StorageReadControl,
    identity: MemoryCaptureIdentity,
}

impl MemoryCaptureSource for ValueIndexes {
    type Frozen = CapturedValueIndexes;

    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        control.check()?;
        Err(StorageBackendError::Other(
            "value-index membership capture requires its original cut".into(),
        ))
    }

    fn freeze_at(
        &self,
        control: &StorageReadControl,
        identity: &MemoryCaptureIdentity,
    ) -> StorageBackendResult<Self::Frozen> {
        control.check()?;
        let indexes = match &self.native {
            Some(native) => native.names.capture(control.memory())?,
            None if self.is_empty() => {
                RetainedMap::new(control.memory()).capture(control.memory())?
            }
            None => return Err(unregistered()),
        };
        control.check()?;
        Ok(CapturedValueIndexes {
            indexes,
            control: control.clone(),
            identity: identity.clone(),
        })
    }
}

impl CapturedValueIndexes {
    pub fn keys(&self) -> StorageBackendResult<impl ExactSizeIterator<Item = &ValueIndexKey>> {
        self.control.check()?;
        Ok(self.indexes.iter_bidirectional().map(|(key, _)| key))
    }

    pub fn next_key(
        &self,
        after: Option<&ValueIndexKey>,
    ) -> StorageBackendResult<Option<&ValueIndexKey>> {
        self.control.check()?;
        Ok(self
            .indexes
            .first_from(after.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded))
            .map(|(key, _)| key))
    }

    pub fn get(
        &self,
        key: &ValueIndexKey,
        cut: &MemoryCapture,
    ) -> StorageBackendResult<Option<Arc<Budgeted<CapturedValueIndex>>>> {
        self.control.check()?;
        cut.check()?;
        if !self.identity.matches(cut) {
            return Err(StorageBackendError::Other(
                "value-index membership belongs to another memory capture".into(),
            ));
        }
        self.indexes
            .get(key)
            .map(|key| key.captured(cut))
            .transpose()
    }
}

fn unregistered() -> StorageBackendError {
    StorageBackendError::Other("value-index collection lacks native source admission".into())
}
