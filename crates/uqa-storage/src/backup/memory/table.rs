//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table lifecycle properties and borrowed field names have one native wire representation.

use crate::{
    image_io::{ImageRead, ImageReader},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};
use std::io::Read;
use uqa_core::{
    image_io::{decode_image, AsyncImageInput, ImageDecoder},
    memory::{Budgeted, BudgetedVec},
};

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
    pub fts_fields: Budgeted<Vec<String>>,
    pub analyzer: Budgeted<Vec<u8>>,
    pub row_type_array_name: Budgeted<Option<String>>,
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

async fn flag(input: &mut AsyncImageInput<'_>) -> StorageBackendResult<bool> {
    match input.array::<1>().await?[0] {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid()),
    }
}
async fn optional_oid(input: &mut AsyncImageInput<'_>) -> StorageBackendResult<Option<u32>> {
    if flag(input).await? {
        Ok(Some(u32::from_le_bytes(input.array().await?)))
    } else {
        Ok(None)
    }
}

/// Reconstruct one exact metadata image with admitted native payloads; reject malformed discriminants, truncated payloads and trailing bytes before returning it.
pub fn read_memory_table_metadata(
    reader: &mut dyn Read,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<DecodedMemoryTableMetadata>> {
    control.check()?;
    decode_image(
        reader,
        control.memory(),
        control.cancellation(),
        async |input| read_memory_table_metadata_from(input).await,
    )
}

/// Retain incomplete table header fields under the caller's current byte/poll limits.
pub fn memory_table_metadata_input(
    control: &StorageReadControl,
) -> StorageBackendResult<ImageDecoder<Budgeted<DecodedMemoryTableMetadata>, StorageBackendError>> {
    control.check()?;
    Ok(ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        |mut reader| async move { read_memory_table_metadata_from(reader.input()).await },
    )?)
}

/// Decode the same native header grammar on an enclosing image continuation.
pub async fn read_memory_table_metadata_from(
    mut input: AsyncImageInput<'_>,
) -> StorageBackendResult<Budgeted<DecodedMemoryTableMetadata>> {
    input.cancellation.check()?;
    let memory = input
        .memory
        .reserve(size_of::<DecodedMemoryTableMetadata>())?;
    if &input.array::<8>().await? != MAGIC {
        return Err(invalid());
    }
    let object_id = input.array().await?;
    let storage_generation = input.array().await?;
    let catalog_oids = if flag(&mut input).await? {
        Some(MemoryRelationIdentities {
            relation: u32::from_le_bytes(input.array().await?),
            row_type: optional_oid(&mut input).await?,
            array_type: optional_oid(&mut input).await?,
            rule: optional_oid(&mut input).await?,
        })
    } else {
        None
    };
    let persistence = input.array::<1>().await?[0];
    let on_commit = input.array::<1>().await?[0];
    if persistence > 1 || on_commit > 2 {
        return Err(invalid());
    }
    let properties = MemoryTableProperties {
        object_id,
        storage_generation,
        catalog_oids,
        persistence,
        on_commit,
        columns_declared: flag(&mut input).await?,
        maps_integer_keys: flag(&mut input).await?,
        statistics: MemoryTableStatistics {
            loaded: flag(&mut input).await?,
            dirty: flag(&mut input).await?,
        },
        next_id: u128::from_le_bytes(input.array().await?),
    };
    let count = input.count().await?;
    let mut field_payloads = input.memory.empty_reservation();
    let mut fts_fields = BudgetedVec::new(input.memory);
    fts_fields.reserve(count)?;
    for _ in 0..count {
        let (field, memory) = input.text().await?.into_parts();
        fts_fields.push(field)?;
        field_payloads.absorb(memory);
    }
    let (fields, mut fields_memory) = fts_fields.into_parts();
    fields_memory.absorb(field_payloads);
    let fts_fields = Budgeted::new(fields, fields_memory);
    let analyzer = input.bytes().await?;
    let row_type_array_name = if flag(&mut input).await? {
        let (name, memory) = input.text().await?.into_parts();
        Budgeted::new(Some(name), memory)
    } else {
        Budgeted::new(None, input.memory.empty_reservation())
    };
    input.end().await?;
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
