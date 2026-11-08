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
    memory::{Budgeted, BudgetedString, BudgetedVec},
    DocId,
};

/// A decoded address in memory mapping version 1. Component owners interpret catalog slots, resource kinds and metadata slots; decoding alone does not certify complete membership.
#[derive(Debug)]
pub enum DecodedMemoryImageAddress {
    Header,
    Catalog(u16),
    FieldAnalyzer {
        table: BudgetedString,
        field: BudgetedString,
    },
    Resource {
        kind: u8,
        name: BudgetedString,
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
    Vector(BudgetedString),
    ValueIndex {
        name: BudgetedString,
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
                table: input.text()?,
                field: input.text()?,
            }
        }
        family if family == MemoryImageFamily::Resource as u16 => {
            DecodedMemoryImageAddress::Resource {
                kind: input.byte()?,
                name: input.text()?,
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
            3 => DecodedMemoryTableUnit::Vector(self.text()?),
            4 => {
                let name = self.text()?;
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
}

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("invalid memory image address".into())
}

#[cfg(test)]
mod tests;
