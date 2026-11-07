//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed foreign records retain bounded child cursors and never project a complete definition.

use super::{
    image, invalid, BoundTableSecurity, CapturedForeignCatalog, RelationIdentity,
    RelationPersistence, SQLError, StorageReadControl, StoredForeignTable, MAGIC,
};
use uqa_core::memory::BudgetedMapIter;
use uqa_sql::catalog::{
    image::{CatalogImageReader, DefinitionImageReader},
    security::foreign_tables::ForeignTableAuthority,
};
use uqa_storage::{
    image_io::{framed_image_reader, ImageRead, ImageReader},
    StorageBackendError, StorageBackendResult,
};

fn error(error: SQLError) -> StorageBackendError {
    match error {
        SQLError::Cancelled(error) => error.into(),
        error => StorageBackendError::backend("foreign catalog image", error),
    }
}

struct Definition<'a>(DefinitionImageReader<'a>);
impl ImageRead for Definition<'_> {
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        self.0.read(output).map_err(error)
    }
}
struct Authority<'a>(CatalogImageReader<'a>);
impl ImageRead for Authority<'_> {
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        self.0.read(output).map_err(error)
    }
}

impl CapturedForeignCatalog {
    /// Borrow the paired durable roots. Initialization admits fixed cursor state; each read is bounded and opens at most one child codec at a time.
    pub fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        if self.tables.len() != self.security.len() {
            return Err(error(invalid("paired membership")));
        }
        let mut header = [0; 64];
        header[..8].copy_from_slice(&MAGIC);
        header[8..16].copy_from_slice(&(self.tables.len() as u64).to_le_bytes());
        ImageReader::new(
            Output {
                tables: self.tables.iter(),
                authorities: self.security.iter(),
                record: None,
                stage: 0,
                options: None,
                option_value: None,
                names: [].iter(),
                header,
                length: 16,
                offset: 0,
                text: &[],
                child: None,
                control,
            },
            control,
        )
    }
}

type Record<'a> = (
    &'a RelationIdentity,
    &'a StoredForeignTable,
    &'a BoundTableSecurity,
);
struct Output<'a, I, J> {
    tables: I,
    authorities: J,
    record: Option<Record<'a>>,
    stage: u8,
    options: Option<BudgetedMapIter<'a, String, String>>,
    option_value: Option<&'a str>,
    names: std::slice::Iter<'a, String>,
    header: [u8; 64],
    length: usize,
    offset: usize,
    text: &'a [u8],
    child: Option<ImageReader<'a>>,
    control: &'a StorageReadControl,
}

impl<'a, I, J> Output<'a, I, J> {
    fn bytes(&mut self, bytes: &[u8]) {
        self.header[self.length..self.length + bytes.len()].copy_from_slice(bytes);
        self.length += bytes.len();
    }
    fn count(&mut self, value: usize) {
        self.bytes(&(value as u64).to_le_bytes());
    }
    fn string(&mut self, value: &'a str) {
        self.count(value.len());
        self.text = value.as_bytes();
    }
    fn optional_string(&mut self, value: Option<&'a str>) {
        self.bytes(&[u8::from(value.is_some())]);
        if let Some(value) = value {
            self.string(value);
        }
    }
    fn optional_oid(&mut self, value: Option<u32>) {
        self.bytes(&[u8::from(value.is_some())]);
        if let Some(value) = value {
            self.bytes(&value.to_le_bytes());
        }
    }
    fn table_identity(&mut self, table: &StoredForeignTable) {
        self.bytes(&[u8::from(table.persistence == RelationPersistence::Unlogged)]);
        self.bytes(&table.object_id);
        self.bytes(&[u8::from(table.catalog_oids.is_some())]);
        if let Some(oids) = table.catalog_oids {
            self.bytes(&oids.relation.to_le_bytes());
            self.optional_oid(oids.row_type);
            self.optional_oid(oids.array_type);
            self.optional_oid(oids.rule);
        }
    }
    fn definition(
        &mut self,
        value: Result<DefinitionImageReader<'a>, SQLError>,
    ) -> StorageBackendResult<()> {
        let source = ImageReader::new(Definition(value.map_err(error)?), self.control)?;
        self.child = Some(framed_image_reader(source, self.control)?);
        Ok(())
    }
}

impl<
        'a,
        I: Iterator<Item = (&'a RelationIdentity, &'a StoredForeignTable)>,
        J: Iterator<Item = (&'a RelationIdentity, &'a ForeignTableAuthority)>,
    > Output<'a, I, J>
{
    fn advance(&mut self) -> StorageBackendResult<bool> {
        self.offset = 0;
        self.length = 0;
        self.text = &[];
        let stage = self.stage;
        if stage == 0 {
            let Some((key, table)) = self.tables.next() else {
                if self.authorities.next().is_some() {
                    return Err(error(invalid("unpaired authority")));
                }
                return Ok(false);
            };
            let Some((authority_key, authority)) = self.authorities.next() else {
                return Err(error(invalid("missing authority")));
            };
            if key != authority_key
                || table.persistence != authority.persistence()
                || table.persistence == RelationPersistence::Temporary
            {
                return Err(error(invalid("paired membership or persistence")));
            }
            self.record = Some((key, table, authority.security()));
            self.stage = 1;
            self.string(&key.schema);
            return Ok(true);
        }
        let (key, table, authority) = self.record.expect("active foreign record");
        self.stage += 1;
        match stage {
            1 => self.string(&key.name),
            2 => self.string(&table.name),
            3 => self.table_identity(table),
            4 => self.optional_string(table.row_type_array_name.as_deref()),
            5 => self.string(&table.server_name),
            6 => {
                self.bytes(&[u8::from(table.server_reference.is_some())]);
                if let Some(reference) = table.server_reference {
                    self.bytes(&reference.oid.to_le_bytes());
                    self.bytes(&reference.object_id);
                }
            }
            7 => {
                self.count(table.options.len());
                self.options = Some(table.options.iter());
            }
            8 => {
                if let Some(value) = self.option_value.take() {
                    self.string(value);
                    self.stage = 8;
                } else if let Some((key, value)) = self.options.as_mut().and_then(Iterator::next) {
                    self.option_value = Some(value);
                    self.string(key);
                    self.stage = 8;
                }
            }
            9 => {
                self.count(table.option_order.len());
                self.names = table.option_order.iter();
            }
            10 => {
                if let Some(name) = self.names.next() {
                    self.string(name);
                    self.stage = 10;
                }
            }
            11 => self.definition(image::columns_image_reader(
                &table.columns,
                self.control.memory(),
                self.control.cancellation(),
            ))?,
            12 => self.definition(image::dropped_attributes_image_reader(
                &table.dropped_attributes,
                self.control.memory(),
                self.control.cancellation(),
            ))?,
            13 => self.definition(image::table_checks_image_reader(
                &table.checks,
                self.control.memory(),
                self.control.cancellation(),
            ))?,
            14 => {
                let source = ImageReader::new(
                    Authority(
                        authority
                            .image_reader(self.control.cancellation())
                            .map_err(error)?,
                    ),
                    self.control,
                )?;
                self.child = Some(framed_image_reader(source, self.control)?);
            }
            15 => {
                self.record = None;
                self.stage = 0;
            }
            _ => unreachable!("foreign image field cursor"),
        }
        Ok(true)
    }
}

impl<'a, I, J> ImageRead for Output<'a, I, J>
where
    I: Iterator<Item = (&'a RelationIdentity, &'a StoredForeignTable)> + Send,
    J: Iterator<Item = (&'a RelationIdentity, &'a ForeignTableAuthority)> + Send,
{
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        let mut written = 0;
        while written < output.len() {
            self.control.check()?;
            if let Some(child) = &mut self.child {
                let count = child.read(&mut output[written..])?;
                written += count;
                if count == 0 {
                    self.child = None;
                }
                continue;
            }
            let bytes = if self.offset < self.length {
                &self.header[self.offset..self.length]
            } else {
                &self.text[self.offset - self.length..]
            };
            if bytes.is_empty() {
                if !self.advance()? {
                    break;
                }
                continue;
            }
            let count = bytes.len().min(output.len() - written).min(4096);
            output[written..written + count].copy_from_slice(&bytes[..count]);
            self.offset += count;
            written += count;
        }
        Ok(written)
    }
}
