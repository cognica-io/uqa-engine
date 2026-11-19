//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    CapturedCatalogIndexes, CatalogIndexRow, ImageRead, ImageReader, RelationIdentity,
    StorageBackendError, StorageBackendResult, StorageReadControl, MAGIC,
};

impl CapturedCatalogIndexes {
    /// Borrow the selected native rows with a fixed-size field cursor. Construction neither scans nor copies encoded definitions.
    pub fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        control.check()?;
        let mut header = [0; 16];
        header[..8].copy_from_slice(&MAGIC);
        header[8..].copy_from_slice(&(self.len() as u64).to_le_bytes());
        ImageReader::new(
            IndexImage {
                entries: self.iter(),
                fields: [None; 7],
                field: 7,
                header,
                header_length: 16,
                offset: 0,
                text: &[],
                identifier: false,
                control,
            },
            control,
        )
    }
}

struct IndexImage<'a, I> {
    entries: I,
    fields: [Option<&'a str>; 7],
    field: usize,
    header: [u8; 16],
    header_length: usize,
    offset: usize,
    text: &'a [u8],
    identifier: bool,
    control: &'a StorageReadControl,
}

impl<'a, I: Iterator<Item = (&'a RelationIdentity, &'a CatalogIndexRow)>> IndexImage<'a, I> {
    fn next_field(&mut self) -> StorageBackendResult<bool> {
        if self.field == self.fields.len() {
            let Some((_, row)) = self.entries.next() else {
                return Ok(false);
            };
            self.fields = [
                Some(&row.relation.schema),
                Some(&row.relation.name),
                Some(&row.index_type),
                Some(&row.table_name),
                Some(&row.columns_json),
                Some(&row.parameters_json),
                row.definition_json.as_deref(),
            ];
            self.field = 0;
        }
        let value = self.fields[self.field];
        self.identifier = self.field < 4;
        if self.identifier && value.is_none_or(str::is_empty) {
            return Err(StorageBackendError::Other(
                "invalid captured index identifier".into(),
            ));
        }
        let tag = usize::from(self.field == 6);
        self.text = value.unwrap_or_default().as_bytes();
        self.header[0] = u8::from(value.is_some());
        self.header_length = tag;
        if value.is_some() {
            self.header[tag..tag + 8].copy_from_slice(&(self.text.len() as u64).to_le_bytes());
            self.header_length += 8;
        }
        self.offset = 0;
        self.field += 1;
        Ok(true)
    }
}

impl<'a, I: Iterator<Item = (&'a RelationIdentity, &'a CatalogIndexRow)> + Send> ImageRead
    for IndexImage<'a, I>
{
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        let mut written = 0;
        while written < output.len() {
            self.control.check()?;
            let bytes = if self.offset < self.header_length {
                &self.header[self.offset..self.header_length]
            } else {
                &self.text[self.offset - self.header_length..]
            };
            if bytes.is_empty() {
                if !self.next_field()? {
                    break;
                }
                continue;
            }
            let count = bytes.len().min(output.len() - written).min(4096);
            if self.identifier && self.offset >= self.header_length && bytes[..count].contains(&0) {
                return Err(StorageBackendError::Other(
                    "invalid captured index identifier".into(),
                ));
            }
            output[written..written + count].copy_from_slice(&bytes[..count]);
            self.offset += count;
            written += count;
        }
        Ok(written)
    }
}
