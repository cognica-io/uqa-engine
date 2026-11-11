//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decoded field revisions and their original names enter native catalog membership together.

use super::{FieldAnalyzerBinding, FieldAnalyzerCatalog, ImageBinding, RuntimeBinding, StoredKey};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use uqa_core::{
    memory::{Budgeted, MemoryBudget, MemoryError, Produced, ProductionControl},
    RelationIdentity,
};

#[cfg(test)]
mod tests;

impl FieldAnalyzerCatalog {
    /// Seek one permanent table's original field bindings without visiting earlier tables. Only the temporary lower-bound name is allocated, under the supplied allowance; yielded bindings borrow this immutable catalog generation.
    pub fn durable_fields_with_control<'a>(
        &'a self,
        table: &'a str,
        control: &StorageReadControl,
    ) -> StorageBackendResult<impl Iterator<Item = (&'a str, &'a FieldAnalyzerBinding)>> {
        control.check()?;
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        let (name, memory) = production.copy_text(table)?.into_parts();
        let start = production.finish((name, String::new()), memory)?;
        let values = self.durable.range_from(std::ops::Bound::Included(&*start));
        control.check()?;
        Ok(values
            .take_while(move |(key, _)| key.0 .0 == table)
            .map(|(key, value)| (key.0 .1.as_str(), &**value)))
    }

    /// Add one decoded permanent field binding without resolving or recompiling its revisions. Input owns the complete key and binding name allocations; immutable catalog copies retain those exact owners. Failure consumes the unpublished candidate.
    pub fn insert_image(
        self,
        key: Budgeted<(String, String)>,
        value: Budgeted<FieldAnalyzerBinding>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let key_bytes = self.validate_image_admission(&key, &value, control)?;
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        let image = ImageBinding::prepare(&value, &production)?;
        let image_key = StoredKey::copy(&key, &production)?;
        self.publish_image(key, value, image_key, image, key_bytes, control)
    }

    fn validate_image_admission<T>(
        &self,
        key: &Budgeted<(String, String)>,
        value: &Budgeted<FieldAnalyzerBinding<T>>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<usize> {
        control.check()?;
        let key_bytes = key
            .0
            .capacity()
            .checked_add(key.1.capacity())
            .ok_or(MemoryError::SizeOverflow)?;
        let value_bytes = size_of::<FieldAnalyzerBinding>()
            .checked_add(value.index.name.as_ref().map_or(0, String::capacity))
            .and_then(|bytes| {
                bytes.checked_add(value.search.name.as_ref().map_or(0, String::capacity))
            })
            .ok_or(MemoryError::SizeOverflow)?;
        if !key.budget().shares_allowance(control.memory())
            || !value.budget().shares_allowance(control.memory())
            || !self.durable.budget().shares_allowance(control.memory())
            || key.reserved_bytes() < key_bytes
            || value.reserved_bytes() < value_bytes
        {
            return Err(invalid(
                "field analyzer image has incomplete or foreign admission",
            ));
        }
        if RelationIdentity::borrowed_reference(&key.0).is_none() || self.get(key).is_some() {
            return Err(invalid(
                "field analyzer image has an invalid or repeated identity",
            ));
        }
        Ok(key_bytes)
    }

    fn publish_image(
        mut self,
        key: Budgeted<(String, String)>,
        value: Budgeted<FieldAnalyzerBinding>,
        image_key: StoredKey,
        image: std::sync::Arc<Produced<ImageBinding>>,
        key_bytes: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let image_bytes = image_key
            .bytes()?
            .checked_add(image.bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        let value = RuntimeBinding::Image(value.into_shared()?);
        self.images.insert(image_key, image, image_bytes)?;
        self.durable
            .insert(StoredKey(Produced::from(key)), value, key_bytes)?;
        control.check()?;
        Ok(self)
    }

    /// Read both private revisions and publish their admitted runtime and encoded roots through the caller's work channel. Failure consumes the unpublished candidate without changing any retained catalog generation.
    pub async fn read_image_from(
        self,
        key: Budgeted<(String, String)>,
        resources: &uqa_analysis::AnalyzerResources,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> StorageBackendResult<Self> {
        let value = FieldAnalyzerBinding::read_owned_image_from(input, resources).await?;
        let control = StorageReadControl::new(input.memory, input.cancellation);
        let key_bytes = self.validate_image_admission(&key, &value, &control)?;
        let (value, image) = ImageBinding::prepare_owned(value, input).await?;
        let table = input.copy_text(&key.0).await?;
        let field = input.copy_text(&key.1).await?;
        let (table, mut memory) = table.into_parts();
        let (field, field_memory) = field.into_parts();
        memory.absorb(field_memory);
        let image_key = StoredKey(Budgeted::new((table, field), memory).into());
        self.publish_image(key, value, image_key, image, key_bytes, &control)
    }

    /// Future field assignments use the destination allowance while unchanged decoded keys, revisions and capture images retain their original owners.
    pub fn into_mutation_budget(mut self, memory: &MemoryBudget) -> Self {
        self.durable = self.durable.into_mutation_budget(memory);
        self.temporary = self.temporary.into_mutation_budget(memory);
        self.images = self.images.into_mutation_budget(memory);
        self
    }
}

fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}
