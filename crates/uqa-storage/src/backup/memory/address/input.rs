//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Strict address decoding retains each complete name and physical identity.

use super::{memory_image_namespace, MemoryImageFamily};
use crate::{
    backup::UnitAddress, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};
use uqa_core::{
    memory::{Budgeted, BudgetedString, BudgetedVec, MemoryError},
    DocId,
};

/// An immutable decoded name. Clones retain the original buffer and admitted shared header without copying its bytes.
#[derive(Clone, Debug)]
pub struct MemoryImageName(std::sync::Arc<Budgeted<String>>);

impl MemoryImageName {
    pub fn from_budgeted(value: Budgeted<String>) -> Result<Self, MemoryError> {
        Ok(Self(value.into_shared()?))
    }
}

impl PartialEq for MemoryImageName {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0) || self.0.as_str() == other.0.as_str()
    }
}

impl Eq for MemoryImageName {}

impl std::ops::Deref for MemoryImageName {
    type Target = str;
    fn deref(&self) -> &str {
        self.0.as_str()
    }
}

impl AsRef<str> for MemoryImageName {
    fn as_ref(&self) -> &str {
        self
    }
}

/// A decoded address in memory mapping version 1. Component owners interpret catalog slots, resource kinds and metadata slots; decoding alone does not certify complete membership.
#[derive(Debug)]
pub enum DecodedMemoryImageAddress {
    Header,
    Catalog(u16),
    FieldAnalyzer {
        table: MemoryImageName,
        field: MemoryImageName,
    },
    Resource {
        kind: u8,
        name: MemoryImageName,
    },
    Table {
        schema: BudgetedString,
        relation: BudgetedString,
        object: [u8; 16],
        generation: [u8; 16],
        unit: DecodedMemoryTableUnit,
    },
}

#[derive(Debug)]
pub enum DecodedMemoryTableUnit {
    Metadata(u8),
    Document(DocId),
    Text(DocId),
    Vector(MemoryImageName),
    ValueIndex {
        name: MemoryImageName,
        document: Option<DocId>,
    },
}

/// Decode exactly one address from the selected memory mapping. Names own their original UTF-8 bytes and admission, including embedded NUL. Unknown structural tags, malformed escapes, truncation, trailing bytes and another mapping's namespace/generation fail.
pub fn read_memory_image_address(
    address: UnitAddress<'_>,
    control: &StorageReadControl,
) -> StorageBackendResult<DecodedMemoryImageAddress> {
    control.check()?;
    if address.namespace != memory_image_namespace() || address.generation != 0 {
        return Err(invalid());
    }
    let mut input = Input {
        bytes: address.key,
        control,
    };
    let result = match address.family {
        family if family == MemoryImageFamily::Header as u16 => DecodedMemoryImageAddress::Header,
        family if family == MemoryImageFamily::Catalog as u16 => {
            DecodedMemoryImageAddress::Catalog(u16::from_be_bytes(input.array()?))
        }
        family if family == MemoryImageFamily::FieldAnalyzer as u16 => {
            DecodedMemoryImageAddress::FieldAnalyzer {
                table: input.name()?,
                field: input.name()?,
            }
        }
        family if family == MemoryImageFamily::Resource as u16 => {
            DecodedMemoryImageAddress::Resource {
                kind: input.byte()?,
                name: input.name()?,
            }
        }
        family if family == MemoryImageFamily::Table as u16 => DecodedMemoryImageAddress::Table {
            schema: input.text()?,
            relation: input.text()?,
            object: input.array()?,
            generation: input.array()?,
            unit: input.table_unit()?,
        },
        _ => return Err(invalid()),
    };
    control.check()?;
    if !input.bytes.is_empty() {
        return Err(invalid());
    }
    Ok(result)
}

struct Input<'a> {
    bytes: &'a [u8],
    control: &'a StorageReadControl,
}

impl Input<'_> {
    fn array<const N: usize>(&mut self) -> StorageBackendResult<[u8; N]> {
        let (value, remaining) = self.bytes.split_at_checked(N).ok_or_else(invalid)?;
        self.bytes = remaining;
        Ok(value.try_into().expect("fixed address field"))
    }

    fn byte(&mut self) -> StorageBackendResult<u8> {
        Ok(self.array::<1>()?[0])
    }

    fn text(&mut self) -> StorageBackendResult<BudgetedString> {
        let mut bytes = BudgetedVec::new(self.control.memory());
        loop {
            self.control.check()?;
            let window = &self.bytes[..self.bytes.len().min(256)];
            let count = window
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(window.len());
            bytes.extend_from_slice(&window[..count])?;
            self.bytes = &self.bytes[count..];
            if count == window.len() && count != 0 {
                continue;
            }
            if self.byte()? != 0 {
                return Err(invalid());
            }
            match self.byte()? {
                0 => break,
                255 => bytes.push(0)?,
                _ => return Err(invalid()),
            }
        }
        self.control.check()?;
        let (bytes, memory) = bytes.into_parts();
        let text = String::from_utf8(bytes).map_err(|_| invalid())?;
        Ok(BudgetedString::from_budgeted(Budgeted::new(text, memory)))
    }

    fn table_unit(&mut self) -> StorageBackendResult<DecodedMemoryTableUnit> {
        Ok(match self.byte()? {
            0 => DecodedMemoryTableUnit::Metadata(self.byte()?),
            1 => DecodedMemoryTableUnit::Document(u64::from_be_bytes(self.array()?)),
            2 => DecodedMemoryTableUnit::Text(u64::from_be_bytes(self.array()?)),
            3 => DecodedMemoryTableUnit::Vector(self.name()?),
            4 => {
                let name = self.name()?;
                let document = match self.byte()? {
                    0 => None,
                    1 => Some(u64::from_be_bytes(self.array()?)),
                    _ => return Err(invalid()),
                };
                DecodedMemoryTableUnit::ValueIndex { name, document }
            }
            _ => return Err(invalid()),
        })
    }

    fn name(&mut self) -> StorageBackendResult<MemoryImageName> {
        let (value, memory) = self.text()?.into_parts();
        Ok(MemoryImageName::from_budgeted(Budgeted::new(
            value, memory,
        ))?)
    }
}

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("invalid memory image address".into())
}

#[cfg(test)]
mod tests;
