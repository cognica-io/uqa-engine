//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native restoration supplies validated partition selection and independently retained child footprints.

use super::{Budgeted, DefinitionEntry, DefinitionKey, DefinitionRegistry, MemoryError};
use uqa_core::memory::MapKeyInput;

impl<K: DefinitionKey, T> DefinitionRegistry<K, T> {
    /// Move already validated native payloads into their original partition. The owning restoration supplies its canonical temporary classification and cached child bytes; immutable keys cannot change partitions. Every comparison suspends through the original work channel.
    pub(crate) async fn insert_native_entry_from<C>(
        &mut self,
        name: Budgeted<K>,
        value: Budgeted<T>,
        children: usize,
        temporary: bool,
        input: &mut C,
    ) -> Result<(), C::Error>
    where
        C: MapKeyInput<K>,
        C::Error: From<MemoryError>,
    {
        assert!(self.budget().shares_allowance(name.budget()));
        assert!(self.budget().shares_allowance(value.budget()));
        let bytes = name
            .payload_bytes()
            .checked_add(value.reserved_bytes())
            .and_then(|bytes| bytes.checked_add(children))
            .ok_or(MemoryError::SizeOverflow)?;
        let (name, key_memory) = name.into_parts();
        let (value, mut memory) = value.into_parts();
        memory.absorb(key_memory);
        let entry = (name, DefinitionEntry::Native(Budgeted::new(value, memory)));
        let selected = if temporary {
            &mut self.temporary
        } else {
            &mut self.durable
        };
        let insertion = selected.prepare_insert_from(entry.0, bytes, input).await?;
        insertion.publish(entry.1);
        Ok(())
    }
}
