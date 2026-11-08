//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table lifecycle properties and borrowed field names have one native wire representation.

use crate::{
    image_io::{ImageInput, ImageRead, ImageReader},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};
use std::io::Read;
use uqa_core::memory::{Budgeted, BudgetedVec};

const MAGIC: &[u8; 8] = b"UQAMTM01";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryRelationIdentities {
    pub relation: u32,
    pub row_type: Option<u32>,
    pub array_type: Option<u32>,
    pub rule: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryTableStatistics {
    pub loaded: bool,
    pub dirty: bool,
}

/// Primitive table properties at the selected cut. Persistence is 0 (permanent) or 1 (unlogged); on-commit is 0 (preserve), 1 (delete rows) or 2 (drop). SQL definitions and evaluated data have their own companion units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryTableProperties {
    pub object_id: [u8; 16],
    pub storage_generation: [u8; 16],
    pub catalog_oids: Option<MemoryRelationIdentities>,
    pub persistence: u8,
    pub on_commit: u8,
    pub columns_declared: bool,
    pub maps_integer_keys: bool,
    pub statistics: MemoryTableStatistics,
    pub next_id: u128,
}

/// Borrowed durable properties. Names, analyzer configuration and the optional array-type name retain their exact bytes and order, including an explicitly present empty name.
pub struct MemoryTableMetadata<'a> {
    pub properties: MemoryTableProperties,
    pub fts_fields: &'a [String],
    pub analyzer: &'a [u8],
    pub row_type_array_name: Option<&'a str>,
}

/// Privately reconstructed properties. Every variable payload and collection node retains its decoding allowance; publication and cross-catalog validation remain with the restored table owner.
pub struct DecodedMemoryTableMetadata {
    pub properties: MemoryTableProperties,
    pub fts_fields: BudgetedVec<Budgeted<String>>,
    pub analyzer: Budgeted<Vec<u8>>,
    pub row_type_array_name: Option<Budgeted<String>>,
}

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("invalid memory table metadata image".into())
}

struct Header {
    bytes: [u8; 128],
    length: usize,
}
impl Header {
    fn new() -> Self {
        Self {
            bytes: [0; 128],
            length: 0,
        }
    }
    fn append(&mut self, bytes: &[u8]) {
        self.bytes[self.length..self.length + bytes.len()].copy_from_slice(bytes);
        self.length += bytes.len();
    }
    fn optional_oid(&mut self, oid: Option<u32>) {
        self.append(&[u8::from(oid.is_some())]);
        if let Some(oid) = oid {
            self.append(&oid.to_le_bytes());
        }
    }
    fn sized(&mut self, bytes: &[u8]) {
        self.length = 0;
        self.append(&(bytes.len() as u64).to_le_bytes());
    }
}

/// Initialize bounded output without visiting fields or copying analyzer/configuration payloads.
pub fn memory_table_metadata_reader<'a>(
    value: MemoryTableMetadata<'a>,
    control: &'a StorageReadControl,
) -> StorageBackendResult<ImageReader<'a>> {
    control.check()?;
    let properties = value.properties;
    if properties.persistence > 1 || properties.on_commit > 2 {
        return Err(invalid());
    }
    let mut header = Header::new();
    header.append(MAGIC);
    header.append(&properties.object_id);
    header.append(&properties.storage_generation);
    header.append(&[u8::from(properties.catalog_oids.is_some())]);
    if let Some(oids) = properties.catalog_oids {
        header.append(&oids.relation.to_le_bytes());
        header.optional_oid(oids.row_type);
        header.optional_oid(oids.array_type);
        header.optional_oid(oids.rule);
    }
    header.append(&[
        properties.persistence,
        properties.on_commit,
        u8::from(properties.columns_declared),
        u8::from(properties.maps_integer_keys),
        u8::from(properties.statistics.loaded),
        u8::from(properties.statistics.dirty),
    ]);
    header.append(&properties.next_id.to_le_bytes());
    header.append(&(value.fts_fields.len() as u64).to_le_bytes());
    ImageReader::new(
        Output {
            value,
            header,
            header_offset: 0,
            payload: &[],
            payload_offset: 0,
            field: 0,
            stage: 0,
            control,
        },
        control,
    )
}

struct Output<'a> {
    value: MemoryTableMetadata<'a>,
    header: Header,
    header_offset: usize,
    payload: &'a [u8],
    payload_offset: usize,
    field: usize,
    stage: u8,
    control: &'a StorageReadControl,
}

impl Output<'_> {
    fn advance(&mut self) -> bool {
        if self.stage == 3 {
            return false;
        }
        self.header_offset = 0;
        self.payload_offset = 0;
        if self.stage == 0 {
            if let Some(field) = self.value.fts_fields.get(self.field) {
                self.field += 1;
                self.payload = field.as_bytes();
                self.header.sized(self.payload);
                return true;
            }
            self.stage = 1;
        }
        match self.stage {
            1 => {
                self.stage = 2;
                self.payload = self.value.analyzer;
                self.header.sized(self.payload);
            }
            2 => {
                self.stage = 3;
                self.header.length = 0;
                self.header
                    .append(&[u8::from(self.value.row_type_array_name.is_some())]);
                self.payload = self.value.row_type_array_name.unwrap_or("").as_bytes();
                if self.value.row_type_array_name.is_some() {
                    self.header
                        .append(&(self.payload.len() as u64).to_le_bytes());
                }
            }
            _ => return false,
        }
        true
    }
}

impl ImageRead for Output<'_> {
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        let mut written = 0;
        while written < output.len() {
            self.control.check()?;
            let (bytes, offset) = if self.header_offset < self.header.length {
                (
                    &self.header.bytes[..self.header.length],
                    &mut self.header_offset,
                )
            } else {
                (self.payload, &mut self.payload_offset)
            };
            let count = (bytes.len() - *offset).min(output.len() - written);
            output[written..written + count].copy_from_slice(&bytes[*offset..*offset + count]);
            written += count;
            *offset += count;
            if count == 0 && !self.advance() {
                break;
            }
        }
        Ok(written)
    }
}

fn flag(input: &mut ImageInput<'_>) -> StorageBackendResult<bool> {
    match input.array::<1>()?[0] {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid()),
    }
}
fn optional_oid(input: &mut ImageInput<'_>) -> StorageBackendResult<Option<u32>> {
    flag(input)?
        .then(|| input.array().map(u32::from_le_bytes))
        .transpose()
}

/// Reconstruct one exact metadata image with admitted native payloads; reject malformed discriminants, truncated payloads and trailing bytes before returning it.
pub fn read_memory_table_metadata(
    reader: &mut dyn Read,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<DecodedMemoryTableMetadata>> {
    control.check()?;
    let memory = control
        .memory()
        .reserve(size_of::<DecodedMemoryTableMetadata>())?;
    let mut input = ImageInput { reader, control };
    if &input.array::<8>()? != MAGIC {
        return Err(invalid());
    }
    let object_id = input.array()?;
    let storage_generation = input.array()?;
    let catalog_oids = if flag(&mut input)? {
        Some(MemoryRelationIdentities {
            relation: u32::from_le_bytes(input.array()?),
            row_type: optional_oid(&mut input)?,
            array_type: optional_oid(&mut input)?,
            rule: optional_oid(&mut input)?,
        })
    } else {
        None
    };
    let persistence = input.array::<1>()?[0];
    let on_commit = input.array::<1>()?[0];
    if persistence > 1 || on_commit > 2 {
        return Err(invalid());
    }
    let properties = MemoryTableProperties {
        object_id,
        storage_generation,
        catalog_oids,
        persistence,
        on_commit,
        columns_declared: flag(&mut input)?,
        maps_integer_keys: flag(&mut input)?,
        statistics: MemoryTableStatistics {
            loaded: flag(&mut input)?,
            dirty: flag(&mut input)?,
        },
        next_id: u128::from_le_bytes(input.array()?),
    };
    let count = input.count()?;
    let mut fts_fields = BudgetedVec::new(control.memory());
    fts_fields.reserve(count)?;
    for _ in 0..count {
        fts_fields.push(input.text()?)?;
    }
    let analyzer = input.bytes()?;
    let row_type_array_name = if flag(&mut input)? {
        Some(input.text()?)
    } else {
        None
    };
    input.end()?;
    Ok(Budgeted::new(
        DecodedMemoryTableMetadata {
            properties,
            fts_fields,
            analyzer,
            row_type_array_name,
        },
        memory,
    ))
}

#[cfg(test)]
mod tests;
