//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private model input retains native parsing, validation and catalog publication.

use super::{ModelCatalog, ModelHandle, ModelSource};
use crate::{
    model_image::{read_model_image_from, retained_model_bytes, ModelRetention},
    DeepModel,
};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Budgeted, MemoryError},
};
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

impl ModelCatalog {
    /// Transfer the exact decoded name, model and reservations to an unpublished native entry. Failed admission consumes this candidate; ordinary publication and capture keep using the same immutable catalog.
    pub fn insert_image(
        self,
        name: Budgeted<String>,
        model: Budgeted<DeepModel>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        self.validate_image(&name, &model, control)?;
        let bytes = retained_model_bytes(&model, || control.check())?;
        self.adopt_image(name, model, bytes, control)
    }

    /// Read and adopt one complete native image using the same grammar and validation as suspended input. Errors consume the unpublished catalog.
    pub fn read_image(
        self,
        name: Budgeted<String>,
        reader: &mut dyn std::io::Read,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        uqa_core::image_io::decode_image(
            reader,
            control.memory(),
            control.cancellation(),
            async |mut input| self.read_image_from(name, &mut input).await,
        )
    }

    /// Retain the original input channel through numeric validation, per-layer footprint accounting and final private publication without retraining or copying decoded weights.
    pub async fn read_image_from(
        self,
        name: Budgeted<String>,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Self> {
        let model = read_model_image_from(input).await?;
        let control = StorageReadControl::new(input.memory, input.cancellation);
        self.validate_image(&name, &model, &control)?;
        let bytes = {
            let mut retention = ModelRetention::new(&model)?;
            loop {
                input.checkpoint().await?;
                if let Some(bytes) = retention.advance(|| control.check())? {
                    break bytes;
                }
            }
        };
        input.checkpoint().await?;
        let candidate = self.adopt_image(name, model, bytes, &control)?;
        input.checkpoint().await?;
        Ok(candidate)
    }

    fn validate_image(
        &self,
        name: &Budgeted<String>,
        model: &Budgeted<DeepModel>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        if !name.budget().shares_allowance(control.memory())
            || !model.budget().shares_allowance(control.memory())
            || !self.values.budget().shares_allowance(control.memory())
            || name.reserved_bytes() < name.capacity()
            || self.contains_key(name.as_str())
        {
            return Err(StorageBackendError::Other(
                "invalid model image admission or identity".into(),
            ));
        }
        Ok(())
    }

    fn adopt_image(
        mut self,
        name: Budgeted<String>,
        model: Budgeted<DeepModel>,
        payload_bytes: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        if model.reserved_bytes() < payload_bytes {
            return Err(StorageBackendError::Other(
                "incomplete model image admission".into(),
            ));
        }
        let model = ModelHandle::from_image(model)?;
        let payload_bytes = payload_bytes
            .checked_add(model.admitted_wrapper_bytes())
            .ok_or(MemoryError::SizeOverflow)?;
        let total = name
            .capacity()
            .checked_add(payload_bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        let (name, memory) = name.into_parts();
        self.values.insert(
            name,
            ModelSource {
                model,
                payload_bytes,
                _name_memory: Some(memory),
            },
            total,
        )?;
        control.check()?;
        Ok(self)
    }
}
