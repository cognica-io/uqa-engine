//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native component addresses preserve membership order and complete object identities.

use crate::{backup::NamespaceId, read_control::StorageReadControl, StorageBackendResult};
use uqa_core::{memory::BudgetedVec, DocId, RelationIdentity};

mod input;
pub use input::{read_memory_image_address, DecodedMemoryImageAddress, DecodedMemoryTableUnit};

/// The namespace identifies this mapping, independently of source or restored history.
pub fn memory_image_namespace() -> NamespaceId {
    NamespaceId::from_bytes(*b"UQAMEMORYIMAGE01").expect("nonzero memory image namespace")
}

/// Component families are emitted in this order; each family has its own native ordered key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum MemoryImageFamily {
    Header = 1,
    Catalog = 2,
    FieldAnalyzer = 3,
    Resource = 4,
    Table = 5,
}

/// One table's metadata and evaluated physical owners, in streaming order.
#[derive(Clone, Copy)]
pub enum MemoryTableUnit<'a> {
    Metadata(u8),
    Document(DocId),
    Text(DocId),
    Vector(&'a str),
    ValueIndex {
        name: &'a str,
        document: Option<DocId>,
    },
}

/// An order-preserving tuple codec: NUL is escaped as 0,255; 0,0 terminates a string. Fixed identities and numeric suffixes follow complete name fields, so prefixes, embedded NUL and arbitrary UTF-8 cannot alias.
pub struct MemoryImageKey(BudgetedVec<u8>);

impl MemoryImageKey {
    pub fn empty(control: &StorageReadControl) -> StorageBackendResult<Self> {
        control.check()?;
        Ok(Self(BudgetedVec::new(control.memory())))
    }

    fn text(&mut self, text: &str, control: &StorageReadControl) -> StorageBackendResult<()> {
        for bytes in text.as_bytes().chunks(256) {
            control.check()?;
            for &byte in bytes {
                self.0.push(byte)?;
                if byte == 0 {
                    self.0.push(255)?;
                }
            }
        }
        self.0.extend_from_slice(&[0, 0])?;
        Ok(())
    }

    pub fn catalog(slot: u16, control: &StorageReadControl) -> StorageBackendResult<Self> {
        let mut key = Self::empty(control)?;
        key.0.extend_from_slice(&slot.to_be_bytes())?;
        Ok(key)
    }

    pub fn field_analyzer(
        table: &str,
        field: &str,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let mut key = Self::empty(control)?;
        key.text(table, control)?;
        key.text(field, control)?;
        Ok(key)
    }

    pub fn resource(
        kind: u8,
        name: &str,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let mut key = Self::empty(control)?;
        key.0.push(kind)?;
        key.text(name, control)?;
        Ok(key)
    }

    pub fn table(
        relation: &RelationIdentity,
        object: [u8; 16],
        generation: [u8; 16],
        unit: MemoryTableUnit<'_>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let mut key = Self::empty(control)?;
        key.text(&relation.schema, control)?;
        key.text(&relation.name, control)?;
        key.0.extend_from_slice(&object)?;
        key.0.extend_from_slice(&generation)?;
        match unit {
            MemoryTableUnit::Metadata(slot) => key.0.extend_from_slice(&[0, slot])?,
            MemoryTableUnit::Document(id) | MemoryTableUnit::Text(id) => {
                key.0
                    .push(if matches!(unit, MemoryTableUnit::Document(_)) {
                        1
                    } else {
                        2
                    })?;
                key.0.extend_from_slice(&id.to_be_bytes())?;
            }
            MemoryTableUnit::Vector(field) => {
                key.0.push(3)?;
                key.text(field, control)?;
            }
            MemoryTableUnit::ValueIndex { name, document } => {
                key.0.push(4)?;
                key.text(name, control)?;
                key.0.push(u8::from(document.is_some()))?;
                if let Some(id) = document {
                    key.0.extend_from_slice(&id.to_be_bytes())?;
                }
            }
        }
        control.check()?;
        Ok(key)
    }

    pub fn into_bytes(self) -> BudgetedVec<u8> {
        self.0
    }
}

#[cfg(test)]
mod tests;
